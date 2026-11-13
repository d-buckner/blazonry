// SPDX-License-Identifier: AGPL-3.0-only
//! The extension API: what a blazonry extension exchanges with the server, and
//! the C ABI a compiled extension exports.
//!
//! A plugin links this crate and nothing else from blazonry. Only JSON text
//! crosses the boundary, so a plugin built after the server was built does not
//! have to match blazonry's types, generics, or allocator (ADR-0002).
//!
//! An extension is a function of its request. It never fetches: the server
//! resolves the provider snapshots and hands them over. That is what keeps this
//! boundary small, and it is why [`Extension::vars`] is synchronous.
//!
//! ```ignore
//! struct Clock;
//!
//! impl blazonry_ext_api::Extension for Clock {
//!     fn manifest(&self) -> serde_json::Value { todo!() }
//!     fn template(&self) -> String { todo!() }
//!     fn vars(&self, req: VarsRequest) -> Result<serde_json::Value, String> { todo!() }
//! }
//!
//! blazonry_ext_api::export_extension!(Clock);
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

/// The ABI this crate implements. A host refuses a plugin that reports any other
/// value.
pub const ABI_VERSION: u32 = 1;

/// The compiler that built this copy of the crate. The host and the plugin each
/// carry their own, so a mismatch shows up at load time instead of as undefined
/// behaviour later.
pub const RUSTC_VERSION: &str = env!("BLAZONRY_EXT_RUSTC");

/// The architecture this copy was built for, as `std` spells it.
pub const ARCH: &str = std::env::consts::ARCH;

/// The symbols a host looks up in a compiled extension.
pub mod symbol {
    /// The names as a dynamic loader wants them: bytes, no trailing NUL.
    /// `fn () -> *mut c_char`. JSON [`Meta`][super::Meta] plus the manifest.
    pub const META: &[u8] = b"blazonry_ext_meta";
    /// `fn () -> *mut c_char`. The template, as raw text.
    pub const TEMPLATE: &[u8] = b"blazonry_ext_template";
    /// `fn (*const c_char) -> *mut c_char`. Vars for one request.
    pub const VARS: &[u8] = b"blazonry_ext_vars";
    /// `fn (*mut c_char)`. Frees a string the extension returned.
    pub const FREE: &[u8] = b"blazonry_ext_free";
}

/// One stored provider snapshot handed to an extension. The server fetched it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotInput {
    pub provider_id: i64,
    /// The name the operator gave the provider; a calendar shows it as the
    /// event source.
    pub name: String,
    pub shape: String,
    pub fetched_at: String,
    /// The shape's canonical payload, envelope-free.
    pub payload: Value,
}

/// Everything one [`Extension::vars`] call may read. This is also the request
/// body between the server and an extension host, so it carries no handles: the
/// clock arrives stamped, and an extension needs no timezone database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VarsRequest {
    pub kind: String,
    /// Stored settings, completed with the manifest's defaults.
    pub settings: Value,
    /// The local wall clock at rebuild time, RFC 3339 with offset.
    pub local_now: String,
    /// The providers resolved for the consumed shape, in resolution order.
    /// Empty only for an extension that consumes no shape, or when a picked
    /// provider was deleted since.
    pub provider_ids: Vec<i64>,
    /// The snapshots those providers published. A provider that has not
    /// refreshed yet is absent, so an extension tells "nothing connected" from
    /// "waiting for a first refresh" by comparing the two lists.
    pub snapshots: Vec<SnapshotInput>,
}

impl VarsRequest {
    /// The stamped instant, with the offset it was stamped at.
    pub fn local_time(&self) -> Result<chrono::DateTime<chrono::FixedOffset>, String> {
        chrono::DateTime::parse_from_rfc3339(&self.local_now)
            .map_err(|e| format!("local_now '{}' is not RFC 3339: {e}", self.local_now))
    }

    /// The local date this rebuild is for.
    pub fn local_date(&self) -> Result<chrono::NaiveDate, String> {
        Ok(self.local_time()?.date_naive())
    }
}

/// What one `vars` call answers with.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VarsResponse {
    Vars(Value),
    Error(String),
}

/// What a compiled extension implements.
pub trait Extension {
    /// The folder's `manifest.json`: kind, label, blurb, consumes, settings.
    fn manifest(&self) -> Value;
    /// The folder's `template.liquid`.
    fn template(&self) -> String;
    /// Liquid vars for one rebuild. The error text reaches the admin UI.
    fn vars(&self, req: VarsRequest) -> Result<Value, String>;
}

/// What a plugin reports before the host calls anything else.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub abi: u32,
    pub rustc: String,
    pub arch: String,
    pub manifest: Value,
}

/// Turns a Rust string into the C string the ABI passes back. The extension owns
/// the allocation, so the host copies the bytes and calls
/// [`symbol::FREE`][symbol::FREE] rather than its own deallocator.
#[doc(hidden)]
pub fn text_to_c(s: String) -> *mut c_char {
    CString::new(s)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[doc(hidden)]
pub fn json_to_c<T: Serialize>(v: &T) -> *mut c_char {
    match serde_json::to_string(v) {
        Ok(s) => text_to_c(s),
        Err(e) => text_to_c(format!(r#"{{"error":"serialization failed: {e}"}}"#)),
    }
}

/// Frees a string an extension returned.
///
/// # Safety
///
/// `p` must be a pointer this extension returned, or null.
#[doc(hidden)]
pub unsafe fn free(p: *mut c_char) {
    if !p.is_null() {
        drop(unsafe { CString::from_raw(p) });
    }
}

/// Decodes a request, calls [`Extension::vars`], and encodes the answer. A
/// panic stays inside this function: an extension that panics reports an error,
/// it does not take the host down. That needs the extension to unwind, so an
/// extension must not build with `panic = "abort"`.
///
/// # Safety
///
/// `req` must be null or a valid NUL-terminated UTF-8 string.
#[doc(hidden)]
pub unsafe fn call_vars<E: Extension + ?Sized>(ext: &E, req: *const c_char) -> *mut c_char {
    let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if req.is_null() {
            return Err("no request".to_string());
        }
        let text = unsafe { CStr::from_ptr(req) }
            .to_str()
            .map_err(|e| format!("request is not UTF-8: {e}"))?;
        let req: VarsRequest =
            serde_json::from_str(text).map_err(|e| format!("request is not a VarsRequest: {e}"))?;
        ext.vars(req)
    }));
    match answer {
        Ok(Ok(vars)) => json_to_c(&VarsResponse::Vars(vars)),
        Ok(Err(e)) => json_to_c(&VarsResponse::Error(e)),
        Err(_) => json_to_c(&VarsResponse::Error(
            "extension panicked; build it without panic = \"abort\"".to_string(),
        )),
    }
}

/// Exports `$ext` as a blazonry extension. Put it at the end of the crate that
/// implements [`Extension`], in a crate built as a `cdylib`.
#[macro_export]
macro_rules! export_extension {
    ($ext:expr) => {
        /// The ABI check plus the manifest, in one call.
        #[unsafe(no_mangle)]
        pub extern "C" fn blazonry_ext_meta() -> *mut ::std::os::raw::c_char {
            $crate::json_to_c(&$crate::Meta {
                abi: $crate::ABI_VERSION,
                rustc: $crate::RUSTC_VERSION.to_string(),
                arch: $crate::ARCH.to_string(),
                manifest: $ext.manifest(),
            })
        }

        /// The folder's template, as raw text.
        #[unsafe(no_mangle)]
        pub extern "C" fn blazonry_ext_template() -> *mut ::std::os::raw::c_char {
            $crate::text_to_c($ext.template())
        }

        /// Vars for one rebuild. `req` is a `VarsRequest` as JSON text.
        #[unsafe(no_mangle)]
        pub extern "C" fn blazonry_ext_vars(
            req: *const ::std::os::raw::c_char,
        ) -> *mut ::std::os::raw::c_char {
            // SAFETY: the host passes a valid NUL-terminated request.
            unsafe { $crate::call_vars(&$ext, req) }
        }

        /// Frees a string this extension returned. The host must call this
        /// instead of its own deallocator, because this extension allocated it.
        #[unsafe(no_mangle)]
        pub extern "C" fn blazonry_ext_free(p: *mut ::std::os::raw::c_char) {
            // SAFETY: the pointer came from this extension's own allocator.
            unsafe { $crate::free(p) }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Counting;
    impl Extension for Counting {
        fn manifest(&self) -> Value {
            json!({"kind": "ok"})
        }
        fn template(&self) -> String {
            "hi".to_string()
        }
        fn vars(&self, req: VarsRequest) -> Result<Value, String> {
            Ok(json!({"n": req.snapshots.len()}))
        }
    }
    struct Boom;
    impl Extension for Boom {
        fn manifest(&self) -> Value {
            json!({})
        }
        fn template(&self) -> String {
            String::new()
        }
        fn vars(&self, _req: VarsRequest) -> Result<Value, String> {
            panic!("boom")
        }
    }

    fn c(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    fn take(p: *mut c_char) -> String {
        assert!(!p.is_null());
        let s = unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_string();
        unsafe { free(p) };
        s
    }

    fn req() -> String {
        json!({"kind": "ok", "settings": {}, "local_now": "2026-11-13T07:15:00-08:00",
               "provider_ids": [], "snapshots": []})
        .to_string()
    }

    #[test]
    fn vars_round_trips_over_the_abi() {
        let r = c(&req());
        let out = take(unsafe { call_vars(&Counting, r.as_ptr()) });
        assert_eq!(out, r#"{"vars":{"n":0}}"#);
    }

    #[test]
    fn a_bad_request_is_an_error_not_a_panic() {
        let r = c("{not json");
        let out: VarsResponse =
            serde_json::from_str(&take(unsafe { call_vars(&Counting, r.as_ptr()) })).unwrap();
        assert!(matches!(out, VarsResponse::Error(_)));
        let out = take(unsafe { call_vars(&Counting, std::ptr::null()) });
        assert!(out.contains("no request"));
    }

    #[test]
    fn a_panicking_extension_reports_an_error() {
        let r = c(&req());
        let out: VarsResponse =
            serde_json::from_str(&take(unsafe { call_vars(&Boom, r.as_ptr()) })).unwrap();
        assert!(matches!(&out, VarsResponse::Error(m) if m.contains("panicked")));
    }

    #[test]
    fn the_request_carries_the_local_clock() {
        let req: VarsRequest = serde_json::from_str(&req()).unwrap();
        assert_eq!(req.local_date().unwrap().to_string(), "2026-11-13");
        assert!(
            req.local_time().unwrap().offset()
                == &chrono::FixedOffset::east_opt(-8 * 3600).unwrap()
        );
    }
}
