//! One-time USB setup, run from a Mac with the iPhone plugged in. After this
//! the phone needs no computer:
//!
//! 1. Creates a dedicated pairing record (the user taps Trust on the phone).
//! 2. Turns on `EnableWifiDebugging`, which lets lockdownd accept that
//!    pairing over the network, so the app can reach it through a loopback
//!    VPN on Wi-Fi or cellular.
//! 3. Checks Developer Mode and reveals its switch in Settings if it is off.
//! 4. Optionally signs Poof.ipa with the user's Apple ID and installs it
//!    (isideload: the same approach as AltServer/Sideloadly/iloader).
//! 5. Copies the pairing file straight into Poof's Documents folder, where the
//!    app imports it on launch.

use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc, time::Duration};

use isideload::{
    anisette::remote_v3::{DEFAULT_ANISETTE_V3_URL, RemoteV3AnisetteProvider},
    auth::{
        apple_account::{TwoFactorCallbackParams, TwoFactorCallbackResponse},
        builder::AppleAccountBuilder,
    },
    dev::developer_session::DeveloperSession,
    sideload::{SideloaderBuilder, builder::MaxCertsBehavior, sideloader::Sideloader},
    util::{callbacks::MaxCertsCallbackBox, fs_storage::FsStorage},
};

use idevice::{
    Idevice, IdeviceError, IdeviceService,
    afc::opcode::AfcFopenMode,
    amfi::AmfiClient,
    house_arrest::HouseArrestClient,
    lockdown::LockdownClient,
    pairing_file::PairingFile,
    provider::{IdeviceProvider, UsbmuxdProvider},
    remote_pairing::{RemotePairingLockdownService, RpPairingFile},
    usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdConnection},
};

pub const APP_BUNDLE_ID: &str = "app.poof.Poof";
/// Host name presented when creating the RPPairing.
const RP_HOST: &str = "Poof";
/// How long to wait for the user to unlock the phone and tap Trust.
const PAIR_TIMEOUT: Duration = Duration::from_secs(180);
/// Name the app looks for in its Documents folder.
const PAIRING_NAME: &str = "poof.mobiledevicepairing";

#[derive(Debug, serde::Serialize)]
pub struct Report {
    pub udid: String,
    pub ios_version: String,
    pub developer_mode: bool,
    /// Poof was signed and installed in this run.
    pub installed: bool,
    /// None when the pairing file reached the app; otherwise why not.
    pub app_push_error: Option<String>,
}

/// USB transport, but authenticating with the pairing record we just made
/// instead of whatever usbmuxd has stored for this computer.
#[derive(Debug)]
struct WithPairing {
    usb: UsbmuxdProvider,
    pairing: PairingFile,
}

impl IdeviceProvider for WithPairing {
    fn connect(
        &self,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = Result<Idevice, IdeviceError>> + Send>> {
        self.usb.connect(port)
    }

    fn label(&self) -> &str {
        self.usb.label()
    }

    fn get_pairing_file(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<PairingFile, IdeviceError>> + Send>> {
        let p = self.pairing.clone();
        Box::pin(async move { Ok(p) })
    }
}

fn e(context: &'static str) -> impl FnOnce(IdeviceError) -> String {
    move |err| format!("{context}: {err}")
}

/// Sign-and-install settings. A free Apple ID re-signs the app as
/// `app.poof.Poof.<TEAMID>` and the signature lasts 7 days; a paid one lasts a year.
pub struct Install {
    pub ipa: PathBuf,
    pub apple_id: String,
    pub password: String,
    /// Where the development certificate and its key are kept between runs,
    /// so a re-install (refresh) does not mint a new certificate each time.
    pub state_dir: PathBuf,
    /// Free accounts may hold two development certificates. When both are
    /// taken, revoke them (breaking apps other tools signed with them) instead
    /// of failing.
    pub revoke_certs: bool,
    /// Anisette (device-identity header) server; None uses isideload's default.
    pub anisette_url: Option<String>,
}

/// `~/Library/Application Support/Poof/signing` on macOS,
/// `~/.local/share/poof/signing` elsewhere.
pub fn default_state_dir() -> PathBuf {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()));
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Poof/signing")
    } else {
        home.join(".local/share/poof/signing")
    }
}

/// What Apple wants for two-factor sign-in, shown to the user.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TwoFactorPrompt {
    pub last_error: Option<String>,
    /// A code was sent by SMS (otherwise to trusted devices).
    pub sms: bool,
    /// The last method failed; the user must pick a method before a code.
    pub unknown: bool,
    pub numbers: Vec<TrustedPhone>,
}

/// A trusted phone number Apple can text a code to.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TrustedPhone {
    pub id: u32,
    pub last_two: String,
}

/// Asks the user for a 2FA answer. `None` aborts. An answer is the code
/// itself, or one of `device` (push to trusted devices), `sms` / `sms:<id>`
/// (text a trusted number), `resend`.
pub type TwoFactor = Arc<
    dyn Fn(TwoFactorPrompt) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync,
>;

fn two_factor_response(p: &TwoFactorPrompt, answer: &str) -> TwoFactorCallbackResponse {
    let answer = answer.trim();
    let first_number = p.numbers.first().map(|n| n.id);
    match answer {
        "device" => TwoFactorCallbackResponse::SendToDevices,
        "resend" if p.unknown => TwoFactorCallbackResponse::SendToDevices,
        "resend" => TwoFactorCallbackResponse::ResendCode,
        "sms" => match first_number {
            Some(id) => TwoFactorCallbackResponse::SendSms(id),
            None => TwoFactorCallbackResponse::SendToDevices,
        },
        a if a.starts_with("sms:") => match a[4..].parse() {
            Ok(id) => TwoFactorCallbackResponse::SendSms(id),
            Err(_) => TwoFactorCallbackResponse::ResendCode,
        },
        // isideload refuses a code while no method is selected; pick devices
        // and it will ask again.
        _ if p.unknown => TwoFactorCallbackResponse::SendToDevices,
        code => TwoFactorCallbackResponse::SubmitCode(code.replace(' ', "")),
    }
}
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

pub struct Options {
    pub udid: Option<String>,
    /// Local copy of the pairing file.
    pub out: PathBuf,
    pub install: Option<Install>,
    pub two_factor: TwoFactor,
    pub log: Log,
}

pub async fn run(opts: Options) -> Result<Report, String> {
    let log = &opts.log;
    let mut mux = UsbmuxdConnection::default().await.map_err(e(
        "usbmuxd (built into macOS; on Linux install and start usbmuxd)",
    ))?;
    let devices = mux.get_devices().await.map_err(e("list devices"))?;
    let device = devices
        .into_iter()
        .filter(|d| matches!(d.connection_type, Connection::Usb))
        .find(|d| opts.udid.as_deref().is_none_or(|u| d.udid == u))
        .ok_or("no iPhone connected over USB")?;
    let buid = mux.get_buid().await.map_err(e("read system BUID"))?;
    log(&format!("Found {}", device.udid));

    let usb = device.to_provider(UsbmuxdAddr::default(), "poof-setup");

    // Sign in first: it is the step most likely to fail (wrong password, 2FA,
    // anisette), and failing here leaves nothing behind on the phone.
    let mut signed_in = match &opts.install {
        Some(inst) => Some(sign_in(inst, &opts.two_factor, log).await?),
        None => None,
    };

    log("Unlock the iPhone and tap Trust (enter the passcode if asked)");
    let host_id = uuid::Uuid::new_v4().to_string().to_uppercase();
    let deadline = tokio::time::Instant::now() + PAIR_TIMEOUT;
    let mut pairing = loop {
        if tokio::time::Instant::now() > deadline {
            return Err(
                "timed out waiting for Trust on the iPhone (unlock it and try again)".into(),
            );
        }
        let mut lockdown = LockdownClient::connect(&usb).await.map_err(e("lockdown"))?;
        let attempt = tokio::time::timeout_at(
            deadline,
            lockdown.pair(host_id.clone(), buid.clone(), Some("Poof")),
        );
        let Ok(result) = attempt.await else { continue };
        match result {
            Ok(p) => break p,
            // Raised while the phone is locked; the dialog appears once it is unlocked.
            Err(IdeviceError::PasswordProtected) => {
                tokio::time::sleep(Duration::from_secs(2)).await
            }
            Err(err) => return Err(format!("pairing: {err}")),
        }
    };
    pairing.udid = Some(device.udid.clone());
    let provider = WithPairing {
        usb,
        pairing: pairing.clone(),
    };

    let mut lockdown = LockdownClient::connect(&provider)
        .await
        .map_err(e("lockdown"))?;
    lockdown
        .start_session(&pairing)
        .await
        .map_err(e("lockdown session"))?;
    let ios_version = lockdown
        .get_value(Some("ProductVersion"), None)
        .await
        .map_err(e("read iOS version"))?
        .as_string()
        .unwrap_or_default()
        .to_string();
    lockdown
        .set_value(
            "EnableWifiDebugging",
            true.into(),
            Some("com.apple.mobile.wireless_lockdown"),
        )
        .await
        .map_err(e("enable network lockdown"))?;
    drop(lockdown);
    log(&format!(
        "Paired with iOS {ios_version}; network access enabled"
    ));

    let lockdown_bytes = pairing
        .clone()
        .serialize()
        .map_err(e("serialize pairing file"))?;
    // iOS 26.4 no longer accepts the lockdown record over the network; the app
    // connects with RPPairing instead (17.4+). One file carries both, like
    // iloader writes it, so it also works with tools that read either half.
    let bytes = if supports_rppairing(&ios_version) {
        log("Creating RPPairing keys (iOS 17.4+)");
        let rp = generate_rppairing(&provider).await?;
        merge_plists(&lockdown_bytes, &rp.to_bytes())?
    } else {
        lockdown_bytes
    };
    std::fs::write(&opts.out, &bytes)
        .map_err(|err| format!("write {}: {err}", opts.out.display()))?;
    log(&format!("Saved pairing file to {}", opts.out.display()));

    let mut amfi = AmfiClient::connect(&provider).await.map_err(e("amfi"))?;
    let developer_mode = amfi
        .get_developer_mode_status()
        .await
        .map_err(e("developer mode status"))?;
    if !developer_mode {
        let _ = amfi.reveal_developer_mode_option_in_ui().await;
    }
    drop(amfi);

    let mut bundle_ids = vec![APP_BUNDLE_ID.to_string()];
    let mut installed = false;
    if let (Some(inst), Some((sideloader, team_id))) = (&opts.install, signed_in.as_mut()) {
        log("Signing and installing Poof (this takes a minute)");
        sideloader
            .install_app(
                &provider,
                inst.ipa.clone(),
                false,
                None::<fn(f32) -> std::future::Ready<()>>,
                None,
            )
            .await
            .map_err(|r| format!("sign/install: {r}"))?;
        let id = format!("{APP_BUNDLE_ID}.{team_id}");
        log(&format!("Installed Poof as {id}"));
        bundle_ids.insert(0, id);
        installed = true;
    }

    // The install takes about a minute; if the phone auto-locked meanwhile,
    // writing into the app's container can fail until it is unlocked.
    let mut app_push_error = None;
    'attempts: for attempt in 0..4 {
        if attempt > 0 {
            log("Unlock the iPhone to finish (copying the pairing file into Poof)");
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        for id in &bundle_ids {
            match push_to_app(&provider, &pairing, id, &bytes).await {
                Ok(()) => {
                    log("Copied pairing file into Poof");
                    app_push_error = None;
                    break 'attempts;
                }
                Err(err) => app_push_error = Some(err),
            }
        }
    }

    Ok(Report {
        udid: device.udid,
        ios_version,
        developer_mode,
        installed,
        app_push_error,
    })
}

/// RPPairing over lockdown exists from iOS 17.4.
fn supports_rppairing(v: &str) -> bool {
    let mut parts = v.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    major > 17 || (major == 17 && minor >= 4)
}

/// Pairs over USB through `com.apple.dt.remotepairingdeviced.lockdown`, the
/// way iloader and idevice_pair do.
async fn generate_rppairing(provider: &WithPairing) -> Result<RpPairingFile, String> {
    let service = RemotePairingLockdownService::connect(provider)
        .await
        .map_err(e("RPPairing service"))?;
    let mut client = service
        .into_client(RP_HOST)
        .map_err(e("RPPairing client"))?;
    let mut rp = RpPairingFile::generate(RP_HOST);
    client
        .connect(&mut rp, || async { "000000".to_string() })
        .await
        .map_err(e("RPPairing"))?;
    Ok(rp)
}

/// Both plists' keys in one XML plist (they don't overlap).
fn merge_plists(a: &[u8], b: &[u8]) -> Result<Vec<u8>, String> {
    let mut merged: plist::Dictionary =
        plist::from_bytes(a).map_err(|err| format!("pairing plist: {err}"))?;
    let extra: plist::Dictionary =
        plist::from_bytes(b).map_err(|err| format!("RPPairing plist: {err}"))?;
    merged.extend(extra);
    let mut out = Vec::new();
    plist::to_writer_xml(&mut out, &merged).map_err(|err| format!("write plist: {err}"))?;
    Ok(out)
}

/// Filesystem-safe name for a server URL.
fn storage_key(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Apple IDs are case-insensitive and stored lowercased; email addresses
/// carry no meaningful surrounding whitespace.
fn normalize_apple_id(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Signs in and picks the developer team. Returns the sideloader ready to
/// install, and the team ID that becomes the installed app's ID suffix.
async fn sign_in(
    inst: &Install,
    two_factor: &TwoFactor,
    log: &Log,
) -> Result<(Sideloader<MaxCertsCallbackBox>, String), String> {
    // reqwest is built without a default TLS crypto provider.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let _ = isideload::init();

    std::fs::create_dir_all(&inst.state_dir).map_err(|err| format!("state dir: {err}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&inst.state_dir, std::fs::Permissions::from_mode(0o700));
    }
    // The email is an input to Apple's SRP proof: a stray space or different
    // capitalisation (autofill) turns into "Enter the correct password".
    let apple_id = normalize_apple_id(&inst.apple_id);

    // The provider's default storage is the working directory, which is `/`
    // (read-only) for an app opened from Finder.
    let server = match inst.anisette_url.as_deref().map(str::trim) {
        Some(url) if !url.is_empty() => url.trim_end_matches('/').to_string(),
        _ => DEFAULT_ANISETTE_V3_URL.to_string(),
    };
    // Each server provisions its own device identity; reusing one server's
    // saved identity with another gets the sign-in rejected.
    let anisette_dir = inst.state_dir.join("anisette").join(storage_key(&server));
    let anisette = RemoteV3AnisetteProvider::default()
        .map_err(|r| format!("anisette: {r}"))?
        .set_url(&server)
        .set_storage(Box::new(FsStorage::new(anisette_dir)));
    log(&format!(
        "Signing in to Apple ID {apple_id} (anisette: {server})"
    ));

    let tf = two_factor.clone();
    let mut account = AppleAccountBuilder::new(&apple_id)
        .anisette_provider(anisette)
        .login(&inst.password, move |p: TwoFactorCallbackParams| {
            let tf = tf.clone();
            let prompt = TwoFactorPrompt {
                last_error: p.last_error,
                sms: p.sms,
                unknown: p.unknown,
                numbers: p
                    .numbers
                    .into_iter()
                    .map(|n| TrustedPhone {
                        id: n.id,
                        last_two: n.last_two_digits,
                    })
                    .collect(),
            };
            async move {
                Ok(match tf(prompt.clone()).await {
                    Some(answer) => two_factor_response(&prompt, &answer),
                    None => TwoFactorCallbackResponse::Abort,
                })
            }
        })
        .await
        .map_err(|r| {
            let msg = format!("Apple ID sign-in: {r}");
            if msg.contains("503") && msg.contains("gsa.apple.com") {
                format!(
                    "Apple is temporarily refusing sign-ins (HTTP 503 from gsa.apple.com), \
                     usually after several failed attempts. Wait at least an hour before \
                     trying again; each retry can extend the block.\n\n{msg}"
                )
            } else if msg.contains("-22406") {
                format!(
                    "{msg}\nIf this password works at account.apple.com, the anisette server \
                     is the likely cause: pick the other one and try again."
                )
            } else {
                msg
            }
        })?;
    let session = DeveloperSession::from_account(&mut account)
        .await
        .map_err(|r| format!("developer session: {r}"))?;

    let mut sideloader = SideloaderBuilder::<MaxCertsCallbackBox>::new(session, apple_id.clone())
        .machine_name("Poof".into())
        .storage(Box::new(FsStorage::new(inst.state_dir.clone())))
        .max_certs_behavior(if inst.revoke_certs {
            MaxCertsBehavior::Revoke
        } else {
            MaxCertsBehavior::Error
        })
        .build();
    let team = sideloader
        .get_team()
        .await
        .map_err(|r| format!("developer team: {r}"))?;
    log(&format!("Signed in (team {})", team.team_id));
    Ok((sideloader, team.team_id))
}

async fn push_to_app(
    provider: &WithPairing,
    pairing: &PairingFile,
    bundle_id: &str,
    bytes: &[u8],
) -> Result<(), String> {
    // Starting house_arrest with the escrow bag lets it write data-protected
    // files even if the phone locks.
    let mut lockdown = LockdownClient::connect(provider)
        .await
        .map_err(e("lockdown"))?;
    let legacy = lockdown
        .start_session(pairing)
        .await
        .map_err(e("lockdown session"))?;
    let (port, ssl) = lockdown
        .start_service_with_escrow("com.apple.mobile.house_arrest", pairing.escrow_bag.clone())
        .await
        .map_err(e("house_arrest"))?;
    let mut idevice = provider.connect(port).await.map_err(e("house_arrest"))?;
    if ssl {
        idevice
            .start_session(pairing, legacy)
            .await
            .map_err(e("house_arrest TLS"))?;
    }
    let mut afc = HouseArrestClient::new(idevice)
        .vend_documents(bundle_id)
        .await
        .map_err(|err| format!("{bundle_id} not installed or not file-sharing enabled ({err})"))?;
    let path = format!("/Documents/{PAIRING_NAME}");
    let mut f = afc
        .open(path, AfcFopenMode::WrOnly)
        .await
        .map_err(e("open in app"))?;
    f.write_entire(bytes).await.map_err(e("write in app"))?;
    f.close().await.map_err(e("close in app"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(unknown: bool) -> TwoFactorPrompt {
        TwoFactorPrompt {
            last_error: None,
            sms: false,
            unknown,
            numbers: vec![TrustedPhone {
                id: 7,
                last_two: "42".into(),
            }],
        }
    }

    #[test]
    fn maps_two_factor_answers() {
        use TwoFactorCallbackResponse as R;
        let p = prompt(false);
        assert!(matches!(two_factor_response(&p, " 123 456 "), R::SubmitCode(c) if c == "123456"));
        assert!(matches!(two_factor_response(&p, "sms"), R::SendSms(7)));
        assert!(matches!(two_factor_response(&p, "sms:9"), R::SendSms(9)));
        assert!(matches!(
            two_factor_response(&p, "device"),
            R::SendToDevices
        ));
        assert!(matches!(two_factor_response(&p, "resend"), R::ResendCode));
        // With no method selected a code would make isideload bail.
        assert!(matches!(
            two_factor_response(&prompt(true), "123456"),
            R::SendToDevices
        ));
    }

    #[test]
    fn storage_key_is_path_safe() {
        assert_eq!(storage_key("https://ani.jkcoxson.com"), "ani.jkcoxson.com");
        assert!(!storage_key("https://h:8080/a/../b").contains('/'));
    }

    #[test]
    fn merges_pairing_halves() {
        let lockdown = include_bytes!("../tests/fixtures/pairing.plist");
        let rp = RpPairingFile::generate("test").to_bytes();
        let merged = merge_plists(lockdown, &rp).unwrap();
        assert!(PairingFile::from_bytes(&merged).is_ok());
        assert!(RpPairingFile::from_bytes(&merged).is_ok());
        assert!(supports_rppairing("17.4") && supports_rppairing("26.4.1"));
        assert!(!supports_rppairing("17.3.1"));
    }

    #[test]
    fn normalizes_apple_id() {
        assert_eq!(
            super::normalize_apple_id("  Me@iCloud.com \n"),
            "me@icloud.com"
        );
    }
}
