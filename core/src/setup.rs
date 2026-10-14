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
    anisette::remote_v3::RemoteV3AnisetteProvider,
    auth::{
        apple_account::{TwoFactorCallbackParams, TwoFactorCallbackResponse},
        builder::AppleAccountBuilder,
    },
    dev::developer_session::DeveloperSession,
    sideload::{SideloaderBuilder, builder::MaxCertsBehavior},
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
    usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdConnection},
};

pub const APP_BUNDLE_ID: &str = "app.poof.Poof";
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

/// Asked for a 2FA code; receives Apple's last error, if any. `None` aborts.
pub type TwoFactor = Arc<
    dyn Fn(Option<String>) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync,
>;
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

    log("Unlock the iPhone and tap Trust (enter the passcode if asked)");
    let host_id = uuid::Uuid::new_v4().to_string().to_uppercase();
    let mut pairing = loop {
        let mut lockdown = LockdownClient::connect(&usb).await.map_err(e("lockdown"))?;
        match lockdown
            .pair(host_id.clone(), buid.clone(), Some("Poof"))
            .await
        {
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

    let bytes = pairing.serialize().map_err(e("serialize pairing file"))?;
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
    if let Some(inst) = &opts.install {
        let id = sideload(&provider, inst, &opts.two_factor, log).await?;
        log(&format!("Installed Poof as {id}"));
        bundle_ids.insert(0, id);
        installed = true;
    }

    let mut app_push_error = None;
    for id in &bundle_ids {
        match push_to_app(&provider, id, &bytes).await {
            Ok(()) => {
                log("Copied pairing file into Poof");
                app_push_error = None;
                break;
            }
            Err(err) => app_push_error = Some(err),
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

async fn sideload(
    provider: &WithPairing,
    inst: &Install,
    two_factor: &TwoFactor,
    log: &Log,
) -> Result<String, String> {
    // reqwest is built without a default TLS crypto provider.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let _ = isideload::init();

    std::fs::create_dir_all(&inst.state_dir).map_err(|err| format!("state dir: {err}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&inst.state_dir, std::fs::Permissions::from_mode(0o700));
    }
    log("Signing in to Apple ID");
    let tf = two_factor.clone();
    // The provider's default storage is the working directory, which is `/`
    // (read-only) for an app opened from Finder.
    let anisette = RemoteV3AnisetteProvider::default()
        .map_err(|r| format!("anisette: {r}"))?
        .set_storage(Box::new(FsStorage::new(inst.state_dir.join("anisette"))));
    let mut account = AppleAccountBuilder::new(&inst.apple_id)
        .anisette_provider(anisette)
        .login(&inst.password, move |p: TwoFactorCallbackParams| {
            let tf = tf.clone();
            async move {
                Ok(match tf(p.last_error).await {
                    Some(code) => TwoFactorCallbackResponse::SubmitCode(code),
                    None => TwoFactorCallbackResponse::Abort,
                })
            }
        })
        .await
        .map_err(|r| format!("Apple ID sign-in: {r}"))?;
    let session = DeveloperSession::from_account(&mut account)
        .await
        .map_err(|r| format!("developer session: {r}"))?;

    let mut sideloader =
        SideloaderBuilder::<MaxCertsCallbackBox>::new(session, inst.apple_id.clone())
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

    log("Signing and installing Poof (this takes a minute)");
    sideloader
        .install_app(
            provider,
            inst.ipa.clone(),
            false,
            None::<fn(f32) -> std::future::Ready<()>>,
            None,
        )
        .await
        .map_err(|r| format!("sign/install: {r}"))?;
    Ok(format!("{APP_BUNDLE_ID}.{}", team.team_id))
}

async fn push_to_app(provider: &WithPairing, bundle_id: &str, bytes: &[u8]) -> Result<(), String> {
    let ha = HouseArrestClient::connect(provider)
        .await
        .map_err(e("house_arrest"))?;
    let mut afc = ha
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
