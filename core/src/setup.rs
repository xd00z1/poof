//! One-time USB setup, run from a Mac (or Linux/Windows with usbmuxd) with the
//! iPhone plugged in. After this the phone needs no computer:
//!
//! 1. Creates a dedicated pairing record (the user taps Trust on the phone).
//! 2. Turns on `EnableWifiDebugging`, which lets lockdownd accept that
//!    pairing over the network, so the app can reach it through a loopback
//!    VPN on Wi-Fi or cellular.
//! 3. Checks Developer Mode and reveals its switch in Settings if it is off.
//! 4. Copies the pairing file straight into Poof's Documents folder, where the
//!    app imports it on launch.

use std::{future::Future, path::Path, pin::Pin, time::Duration};

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

#[derive(Debug)]
pub struct Report {
    pub udid: String,
    pub ios_version: String,
    pub developer_mode: bool,
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

pub async fn run(
    udid: Option<&str>,
    out: &Path,
    bundle_id: &str,
    log: impl Fn(&str),
) -> Result<Report, String> {
    let mut mux = UsbmuxdConnection::default()
        .await
        .map_err(e("usbmuxd (built into macOS; on Linux install and start usbmuxd)"))?;
    let devices = mux.get_devices().await.map_err(e("list devices"))?;
    let device = devices
        .into_iter()
        .filter(|d| matches!(d.connection_type, Connection::Usb))
        .find(|d| udid.is_none_or(|u| d.udid == u))
        .ok_or("no iPhone connected over USB")?;
    let buid = mux.get_buid().await.map_err(e("read system BUID"))?;
    log(&format!("Found {}", device.udid));

    let usb = device.to_provider(UsbmuxdAddr::default(), "poof-setup");

    log("Unlock the iPhone and tap Trust (enter the passcode if asked)...");
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
    log(&format!(
        "Paired with iOS {ios_version}; network access enabled"
    ));

    let bytes = pairing.serialize().map_err(e("serialize pairing file"))?;
    std::fs::write(out, &bytes).map_err(|err| format!("write {}: {err}", out.display()))?;
    log(&format!("Saved pairing file to {}", out.display()));

    let developer_mode = match AmfiClient::connect(&provider).await {
        Ok(mut amfi) => match amfi.get_developer_mode_status().await {
            Ok(true) => true,
            Ok(false) => {
                let _ = amfi.reveal_developer_mode_option_in_ui().await;
                false
            }
            Err(err) => return Err(format!("developer mode status: {err}")),
        },
        Err(err) => return Err(format!("amfi: {err}")),
    };

    let app_push_error = push_to_app(&provider, bundle_id, &bytes).await.err();
    if app_push_error.is_none() {
        log("Copied pairing file into Poof");
    }

    Ok(Report {
        udid: device.udid,
        ios_version,
        developer_mode,
        app_push_error,
    })
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
