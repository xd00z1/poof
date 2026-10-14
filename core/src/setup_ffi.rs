//! C ABI for the Mac setup app. Like the engine API, every call returns
//! immediately; the app polls `poof_setup_status_json`.

use std::{
    ffi::{CString, c_char},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::Serialize;
use tokio::sync::oneshot;

use crate::{
    POOF_ERR_ARG, POOF_ERR_RUNTIME, POOF_OK, runtime,
    setup::{self, Install, Options, Report, TwoFactor},
    str_arg,
};

pub const POOF_ERR_BUSY: i32 = -4;

#[derive(Default, Serialize)]
struct State {
    running: bool,
    log: Vec<String>,
    /// Apple is waiting for a two-factor code.
    needs_code: bool,
    code_error: Option<String>,
    report: Option<Report>,
    error: Option<String>,
    #[serde(skip)]
    code_tx: Option<oneshot::Sender<Option<String>>>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> T {
    f(STATE.lock().unwrap().get_or_insert_with(State::default))
}

/// Runs the one-time setup on the first USB iPhone. With a null `apple_id`
/// it only pairs (Poof already installed); otherwise it also signs and
/// installs the IPA at `ipa_path`. `anisette_url` may be null (default server).
///
/// # Safety
/// String arguments must be null or valid NUL-terminated UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn poof_setup_start(
    apple_id: *const c_char,
    password: *const c_char,
    ipa_path: *const c_char,
    pairing_out: *const c_char,
    revoke_certs: bool,
    anisette_url: *const c_char,
) -> i32 {
    let Some(out) = (unsafe { str_arg(pairing_out) }) else {
        return POOF_ERR_ARG;
    };
    let install = match unsafe { (str_arg(apple_id), str_arg(password), str_arg(ipa_path)) } {
        (None, _, _) => None,
        (Some(id), Some(pw), Some(ipa)) => Some(Install {
            ipa: PathBuf::from(ipa),
            apple_id: id.to_string(),
            password: pw.to_string(),
            state_dir: setup::default_state_dir(),
            revoke_certs,
            anisette_url: unsafe { str_arg(anisette_url) }.map(str::to_string),
        }),
        _ => return POOF_ERR_ARG,
    };
    let Some(rt) = runtime() else {
        return POOF_ERR_RUNTIME;
    };
    if with_state(|s| s.running) {
        return POOF_ERR_BUSY;
    }
    with_state(|s| {
        *s = State {
            running: true,
            ..Default::default()
        }
    });

    let two_factor: TwoFactor = Arc::new(|last_error| {
        Box::pin(async move {
            let (tx, rx) = oneshot::channel();
            with_state(|s| {
                s.needs_code = true;
                s.code_error = last_error;
                s.code_tx = Some(tx);
            });
            let code = rx.await.ok().flatten();
            with_state(|s| s.needs_code = false);
            code
        })
    });
    let opts = Options {
        udid: None,
        out: PathBuf::from(out),
        install,
        two_factor,
        log: Arc::new(|m| with_state(|s| s.log.push(m.to_string()))),
    };
    rt.spawn(async move {
        let res = setup::run(opts).await;
        with_state(|s| {
            s.running = false;
            s.needs_code = false;
            s.code_tx = None;
            match res {
                Ok(r) => s.report = Some(r),
                Err(e) => s.error = Some(e),
            }
        });
    });
    POOF_OK
}

/// Answers a pending two-factor prompt. Null or empty aborts the sign-in.
///
/// # Safety
/// `code` must be null or a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn poof_setup_submit_code(code: *const c_char) -> i32 {
    let code = unsafe { str_arg(code) }.map(|c| c.trim().to_string());
    match with_state(|s| s.code_tx.take()) {
        Some(tx) => {
            let _ = tx.send(code);
            POOF_OK
        }
        None => POOF_ERR_ARG,
    }
}

/// Setup progress as JSON. Free with `poof_string_free`.
#[unsafe(no_mangle)]
pub extern "C" fn poof_setup_status_json() -> *mut c_char {
    let json = with_state(|s| serde_json::to_string(s).unwrap_or_default());
    CString::new(json)
        .map(CString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

/// UDIDs of iPhones connected over USB, as a JSON array. Blocks for at most
/// two seconds. Free with `poof_string_free`.
#[unsafe(no_mangle)]
pub extern "C" fn poof_usb_devices_json() -> *mut c_char {
    let udids: Vec<String> = runtime()
        .and_then(|rt| {
            rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(2), async {
                    let mut mux = idevice::usbmuxd::UsbmuxdConnection::default().await.ok()?;
                    let devices = mux.get_devices().await.ok()?;
                    Some(
                        devices
                            .into_iter()
                            .filter(|d| {
                                matches!(d.connection_type, idevice::usbmuxd::Connection::Usb)
                            })
                            .map(|d| d.udid)
                            .collect(),
                    )
                })
                .await
                .ok()
                .flatten()
            })
        })
        .unwrap_or_default();
    CString::new(serde_json::to_string(&udids).unwrap_or_default())
        .map(CString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}
