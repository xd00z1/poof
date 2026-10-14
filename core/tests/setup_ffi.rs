//! The Mac app's C ABI, end to end without a phone attached.
#![cfg(feature = "desktop")]

use std::{
    ffi::{CStr, CString},
    time::Duration,
};

use poof_core::{POOF_ERR_ARG, POOF_OK, poof_string_free, setup_ffi::*};

fn status() -> serde_json::Value {
    let p = poof_setup_status_json();
    let v = serde_json::from_str(unsafe { CStr::from_ptr(p) }.to_str().unwrap()).unwrap();
    unsafe { poof_string_free(p) };
    v
}

#[test]
fn setup_reports_failure_without_device() {
    let out = CString::new(
        std::env::temp_dir()
            .join("poof-ffi.pairing")
            .to_str()
            .unwrap(),
    )
    .unwrap();
    let id = CString::new("someone@example.com").unwrap();

    // Apple ID without password or IPA is rejected up front.
    let rc = unsafe {
        poof_setup_start(
            id.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            out.as_ptr(),
            false,
            std::ptr::null(),
        )
    };
    assert_eq!(rc, POOF_ERR_ARG);

    // Pair-only run starts, then fails cleanly because nothing is plugged in.
    let rc = unsafe {
        poof_setup_start(
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            out.as_ptr(),
            false,
            std::ptr::null(),
        )
    };
    assert_eq!(rc, POOF_OK);
    let mut s = status();
    for _ in 0..100 {
        if s["running"] == false {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
        s = status();
    }
    assert_eq!(s["running"], false);
    assert!(
        s["error"].as_str().unwrap().contains("usbmuxd")
            || s["error"].as_str().unwrap().contains("USB"),
        "{s}"
    );
    assert_eq!(
        unsafe { poof_setup_submit_code(std::ptr::null()) },
        POOF_ERR_ARG
    );

    let p = poof_usb_devices_json();
    assert_eq!(unsafe { CStr::from_ptr(p) }.to_str().unwrap(), "[]");
    unsafe { poof_string_free(p) };
}
