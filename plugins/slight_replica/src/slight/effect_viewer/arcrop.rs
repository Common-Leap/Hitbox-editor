//! Resolve Arcropolis API exports at runtime (`nn::ro::LookupSymbol`).
//!
//! Ported verbatim from the original effect viewer's `arcrop.rs` (the working LiveEdit
//! build). Using LookupSymbol instead of an `extern "C"` block means a missing libarcropolis
//! degrades gracefully (init() returns false) instead of failing to load the whole plugin.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;

use parking_lot::Mutex;

type RegisterCallbackFn = extern "C" fn(u64, usize, DiskCallbackFn);
type RegisterStreamFn = extern "C" fn(u64, StreamCallbackFn);
type IsFileLoadedFn = extern "C" fn(u64) -> bool;
type LoadFileFn = extern "C" fn(u64, *mut u8, usize, &mut usize) -> bool;

pub type DiskCallbackFn = extern "C" fn(u64, *mut u8, usize, &mut usize) -> bool;
/// Stream callback: (hash, out_path_buf, out_len) → true when we supplied a path.
pub type StreamCallbackFn = extern "C" fn(u64, *mut u8, &mut usize) -> bool;

struct Api {
    register_callback: RegisterCallbackFn,
    register_stream: RegisterStreamFn,
    is_file_loaded: IsFileLoadedFn,
    load_file: LoadFileFn,
}

static API: LazyLock<Mutex<Option<Api>>> = LazyLock::new(|| Mutex::new(None));
static LOGGED_FAIL: AtomicBool = AtomicBool::new(false);

unsafe fn lookup<T>(name: &[u8]) -> Option<T> {
    let mut addr = 0usize;
    let rc = skyline::nn::ro::LookupSymbol(&mut addr, name.as_ptr() as *const _);
    if rc != 0 || addr == 0 {
        return None;
    }
    Some(std::mem::transmute_copy(&addr))
}

pub fn init() -> bool {
    if API.lock().is_some() {
        return true;
    }
    unsafe {
        let register_callback: Option<RegisterCallbackFn> = lookup(b"arcrop_register_callback\0");
        let register_stream: Option<RegisterStreamFn> =
            lookup(b"arcrop_register_callback_with_path\0");
        let is_file_loaded: Option<IsFileLoadedFn> = lookup(b"arcrop_is_file_loaded\0");
        let load_file: Option<LoadFileFn> = lookup(b"arcrop_load_file\0");
        match (
            register_callback,
            register_stream,
            is_file_loaded,
            load_file,
        ) {
            (Some(rc), Some(rs), Some(ifl), Some(lf)) => {
                *API.lock() = Some(Api {
                    register_callback: rc,
                    register_stream: rs,
                    is_file_loaded: ifl,
                    load_file: lf,
                });
                skyline::println!("[SLight] Arcropolis API resolved");
                crate::slight::diag::note("arcropolis API resolved");
                true
            }
            _ => {
                if !LOGGED_FAIL.swap(true, Ordering::SeqCst) {
                    skyline::println!("[SLight] Arcropolis API missing (is libarcropolis loaded?)");
                    crate::slight::diag::note("arcropolis API missing");
                }
                false
            }
        }
    }
}

pub fn register_stream(hash: u64, cb: StreamCallbackFn) -> bool {
    if !init() {
        return false;
    }
    let api = API.lock();
    let Some(api) = api.as_ref() else {
        return false;
    };
    (api.register_stream)(hash, cb);
    true
}

#[allow(dead_code)]
pub fn register_disk(hash: u64, max_size: usize, cb: DiskCallbackFn) -> bool {
    if !init() {
        return false;
    }
    let api = API.lock();
    let Some(api) = api.as_ref() else {
        return false;
    };
    (api.register_callback)(hash, max_size, cb);
    true
}

#[allow(dead_code)]
pub fn is_file_loaded(hash: u64) -> bool {
    if !init() {
        return false;
    }
    let api = API.lock();
    let Some(api) = api.as_ref() else {
        return false;
    };
    (api.is_file_loaded)(hash)
}

pub fn load_file(hash: u64, buffer: &mut [u8]) -> Option<usize> {
    if !init() {
        return None;
    }
    let api = API.lock();
    let Some(api) = api.as_ref() else { return None };
    let mut out = 0usize;
    let ok = (api.load_file)(hash, buffer.as_mut_ptr(), buffer.len(), &mut out);
    if ok {
        Some(out)
    } else {
        None
    }
}
