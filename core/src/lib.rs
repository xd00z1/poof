//! poof-core: location simulation for iOS devices through Apple's developer
//! services, with the keep-alive iOS 17+ needs.
//!
//! The C ABI below is what the iOS app links against (see `include/poof.h`).
//! All calls are non-blocking: they post a command to the engine running on a
//! private tokio runtime and return. Poll `poof_status_json` for the outcome.

pub mod engine;
pub mod geo;
#[cfg(feature = "desktop")]
pub mod setup;
#[cfg(feature = "desktop")]
pub mod setup_ffi;

use std::{
    ffi::{CStr, CString, c_char},
    path::PathBuf,
    sync::Mutex,
};

use engine::{Command, Config, Engine};
use geo::{Coord, Mover, Route};

pub const POOF_OK: i32 = 0;
pub const POOF_ERR_ARG: i32 = -1;
pub const POOF_ERR_NOT_RUNNING: i32 = -2;
pub const POOF_ERR_RUNTIME: i32 = -3;

/// Runtime for the Mac setup calls; each engine runs its own (see `Engine::spawn`).
#[cfg(feature = "desktop")]
pub(crate) fn runtime() -> Option<&'static tokio::runtime::Runtime> {
    static RT: std::sync::OnceLock<Option<tokio::runtime::Runtime>> = std::sync::OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("poof")
            .enable_all()
            .build()
            .ok()
    })
    .as_ref()
}

static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);

/// # Safety
/// `p` must be null or a valid NUL-terminated string.
pub(crate) unsafe fn str_arg<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(p) }
        .to_str()
        .ok()
        .filter(|s| !s.is_empty())
}

fn with_engine(cmd: Command) -> i32 {
    match ENGINE.lock().unwrap().as_ref() {
        Some(e) if !e.is_finished() && e.send(cmd) => POOF_OK,
        _ => POOF_ERR_NOT_RUNNING,
    }
}

/// Starts (or restarts) the engine. `ddi_dir` may be null.
///
/// # Safety
/// String arguments must be null or valid NUL-terminated UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn poof_start(
    pairing_file: *const c_char,
    device_ip: *const c_char,
    ddi_dir: *const c_char,
    refresh_secs: u32,
) -> i32 {
    let (Some(pairing), Some(ip)) = (unsafe { str_arg(pairing_file) }, unsafe {
        str_arg(device_ip)
    }) else {
        return POOF_ERR_ARG;
    };
    let Ok(device_ip) = ip.parse() else {
        return POOF_ERR_ARG;
    };
    let cfg = Config {
        pairing_file: PathBuf::from(pairing),
        device_ip,
        ddi_dir: unsafe { str_arg(ddi_dir) }.map(PathBuf::from),
        refresh_secs: refresh_secs as u64,
    };

    let mut slot = ENGINE.lock().unwrap();
    if let Some(old) = slot.take() {
        old.send(Command::Shutdown);
    }
    *slot = Some(Engine::spawn(cfg));
    POOF_OK
}

#[unsafe(no_mangle)]
pub extern "C" fn poof_set_location(lat: f64, lon: f64) -> i32 {
    match Coord::new(lat, lon) {
        Some(c) => with_engine(Command::Set(c)),
        None => POOF_ERR_ARG,
    }
}

/// Walks a route. `route_json` is
/// `{"points":[{"lat":..,"lon":..},...],"speed_mps":1.4,"loop":false}`.
///
/// # Safety
/// `route_json` must be null or a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn poof_walk(route_json: *const c_char) -> i32 {
    let Some(json) = (unsafe { str_arg(route_json) }) else {
        return POOF_ERR_ARG;
    };
    match serde_json::from_str::<Route>(json) {
        Ok(route) if Mover::new(route.clone()).is_some() => with_engine(Command::Walk(route)),
        _ => POOF_ERR_ARG,
    }
}

/// Returns the device to its real GPS while keeping the engine connected.
#[unsafe(no_mangle)]
pub extern "C" fn poof_clear() -> i32 {
    with_engine(Command::Clear)
}

/// Clears the location and shuts the engine down.
#[unsafe(no_mangle)]
pub extern "C" fn poof_stop() -> i32 {
    match ENGINE.lock().unwrap().take() {
        Some(e) => {
            e.send(Command::Shutdown);
            POOF_OK
        }
        None => POOF_ERR_NOT_RUNNING,
    }
}

/// Engine status as JSON (see `engine::Status`), or `{"phase":"stopped"}`
/// when not running. Free with `poof_string_free`.
#[unsafe(no_mangle)]
pub extern "C" fn poof_status_json() -> *mut c_char {
    let json = match ENGINE.lock().unwrap().as_ref() {
        Some(e) => serde_json::to_string(&e.status()).unwrap_or_default(),
        None => r#"{"phase":"stopped"}"#.to_string(),
    };
    CString::new(json)
        .map(CString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

/// # Safety
/// `s` must be null or a pointer returned by this library, freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn poof_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}
