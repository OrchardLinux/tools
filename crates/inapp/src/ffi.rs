//! The engine as the helper calls it from Swift: JSON in, JSON out, one
//! engine per process.
use crate::engine::{Config, Engine};
use std::ffi::{c_char, CStr, CString};
use std::sync::{Arc, OnceLock};

static ENGINE: OnceLock<Arc<Engine>> = OnceLock::new();

/// # Safety
/// `text` is null or a NUL-terminated string.
unsafe fn text<'a>(text: *const c_char) -> &'a str {
    if text.is_null() {
        return "";
    }
    // SAFETY: the caller's contract.
    unsafe { CStr::from_ptr(text) }.to_str().unwrap_or("")
}

fn give(text: String) -> *mut c_char {
    CString::new(text.replace('\0', "")).map_or(std::ptr::null_mut(), CString::into_raw)
}

/// Starts the engine with its configuration (`engine::Config`, as JSON).
/// `foreign` is called, from any thread, with each effect the helper carries
/// out. Returns null, or why it did not start (to free with `oi_free`).
///
/// # Safety
/// `config` is a NUL-terminated string; `foreign` stays callable for the
/// life of the process and does not keep the string it is given.
#[no_mangle]
pub unsafe extern "C" fn oi_start(config: *const c_char, foreign: extern "C" fn(*const c_char)) -> *mut c_char {
    // SAFETY: the caller's contract.
    let config: Config = match serde_json::from_str(unsafe { text(config) }) {
        Ok(config) => config,
        Err(e) => return give(format!("unreadable configuration: {e}")),
    };
    let call = move |effect: &str| {
        if let Ok(effect) = CString::new(effect) {
            foreign(effect.as_ptr());
        }
    };
    if ENGINE.set(Engine::start(config, Box::new(call))).is_err() {
        return give("the engine runs already".into());
    }
    std::ptr::null_mut()
}

/// One request of the app; its answer, to free with `oi_free`.
///
/// # Safety
/// `request` is a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn oi_request(request: *const c_char) -> *mut c_char {
    let refused = |message: &str| serde_json::json!({"error": {"message": message}});
    // SAFETY: the caller's contract.
    let answer = match (ENGINE.get(), serde_json::from_str(unsafe { text(request) })) {
        (Some(engine), Ok(request)) => engine.request(&request),
        (None, _) => refused("The installer has not started."),
        (_, Err(_)) => refused("A request is JSON."),
    };
    give(answer.to_string())
}

/// The resize engine's snapshot, as it encodes it.
///
/// # Safety
/// `snapshot` is a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn oi_storage(snapshot: *const c_char) {
    // SAFETY: the caller's contract.
    if let (Some(engine), Ok(snapshot)) = (ENGINE.get(), serde_json::from_str(unsafe { text(snapshot) })) {
        engine.storage(&snapshot);
    }
}

/// The resize engine took the split the flow asked for, under this id.
///
/// # Safety
/// `operation` is a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn oi_split_started(operation: *const c_char) {
    if let Some(engine) = ENGINE.get() {
        // SAFETY: the caller's contract.
        engine.split_started(unsafe { text(operation) });
    }
}

/// The resize engine refused the split, with its reason.
///
/// # Safety
/// `message` is a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn oi_split_refused(message: *const c_char) {
    if let Some(engine) = ENGINE.get() {
        // SAFETY: the caller's contract.
        engine.split_refused(unsafe { text(message) });
    }
}

/// A line for the helper's log.
///
/// # Safety
/// `line` is a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn oi_log(line: *const c_char) {
    if let Some(engine) = ENGINE.get() {
        // SAFETY: the caller's contract.
        engine.say(unsafe { text(line) });
    }
}

/// Frees a string this library returned.
///
/// # Safety
/// `text` is null or came from this library and was not freed before.
#[no_mangle]
pub unsafe extern "C" fn oi_free(text: *mut c_char) {
    if !text.is_null() {
        // SAFETY: the caller's contract.
        drop(unsafe { CString::from_raw(text) });
    }
}
