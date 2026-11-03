//! The handful of GL entry points we call directly, resolved once through
//! `eglGetProcAddress` (extensions) with a `dlsym` on libGL fallback (core)
//! — the same loader discipline `mpv-engine` documents for Linux shells.
//!
//! Pure FFI plumbing: no GTK, no widget state. The widget side lives in
//! the crate root.

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::sync::OnceLock;

pub const FRAMEBUFFER_BINDING: u32 = 0x8CA6;
pub const FRAMEBUFFER: u32 = 0x8D40;
pub const COLOR_BUFFER_BIT: u32 = 0x4000;

const RTLD_LAZY: c_int = 1;

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// Opaque `dlopen` handle; sharing across threads is safe (the dynamic
/// loader is thread-safe).
struct LibHandle(*mut c_void);
unsafe impl Send for LibHandle {}
unsafe impl Sync for LibHandle {}

type EglGetProcAddress = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type GetIntegervFn = unsafe extern "C" fn(pname: u32, params: *mut i32);
type ClearFn = unsafe extern "C" fn(mask: u32);
type ClearColorFn = unsafe extern "C" fn(r: f32, g: f32, b: f32, a: f32);
type BindFramebufferFn = unsafe extern "C" fn(target: u32, framebuffer: u32);

fn libgl() -> *mut c_void {
    static HANDLE: OnceLock<LibHandle> = OnceLock::new();
    HANDLE
        .get_or_init(|| unsafe {
            let h = dlopen(c"libGL.so.1".as_ptr(), RTLD_LAZY);
            LibHandle(if h.is_null() {
                dlopen(c"libGL.so".as_ptr(), RTLD_LAZY)
            } else {
                h
            })
        })
        .0
}

fn libegl() -> *mut c_void {
    static HANDLE: OnceLock<LibHandle> = OnceLock::new();
    HANDLE
        .get_or_init(|| unsafe { LibHandle(dlopen(c"libEGL.so.1".as_ptr(), RTLD_LAZY)) })
        .0
}

fn egl_sym<T>(name: &str) -> Option<T> {
    let cname = CString::new(name).ok()?;
    let p = unsafe { dlsym(libegl(), cname.as_ptr()) };
    if p.is_null() {
        return None;
    }
    Some(unsafe { std::mem::transmute_copy(&p) })
}

fn egl_get_proc_address() -> Option<EglGetProcAddress> {
    static FN: OnceLock<Option<EglGetProcAddress>> = OnceLock::new();
    *FN.get_or_init(|| egl_sym::<EglGetProcAddress>("eglGetProcAddress"))
}

/// Resolve a GL symbol; null if unavailable.
pub fn resolve(name: &str) -> *mut c_void {
    let Some(cname) = CString::new(name).ok() else {
        return std::ptr::null_mut();
    };
    unsafe {
        if let Some(get) = egl_get_proc_address() {
            let p = get(cname.as_ptr());
            // eglGetProcAddress reports failure with the sentinels
            // 1, 2, 3, -1 rather than null.
            let n = p as isize;
            if !p.is_null() && n > 3 && n != -1 {
                return p;
            }
        }
        dlsym(libgl(), cname.as_ptr())
    }
}

fn sym<T>(name: &str) -> Option<T> {
    let p = resolve(name);
    if p.is_null() {
        return None;
    }
    Some(unsafe { std::mem::transmute_copy(&p) })
}

pub fn get_integer(pname: u32) -> Option<i32> {
    let f: GetIntegervFn = sym("glGetIntegerv")?;
    let mut value = 0i32;
    unsafe { f(pname, &mut value) };
    Some(value)
}

pub fn bind_framebuffer(fbo: i32) -> Option<()> {
    let f: BindFramebufferFn = sym("glBindFramebuffer")?;
    unsafe { f(FRAMEBUFFER, fbo as u32) };
    Some(())
}

pub fn clear(mask: u32) -> Option<()> {
    let f: ClearFn = sym("glClear")?;
    unsafe { f(mask) };
    Some(())
}

pub fn clear_color(r: f32, g: f32, b: f32, a: f32) -> Option<()> {
    let f: ClearColorFn = sym("glClearColor")?;
    unsafe { f(r, g, b, a) };
    Some(())
}
