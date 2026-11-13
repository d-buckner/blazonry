// SPDX-License-Identifier: AGPL-3.0-only
//! The extension host: the loader that turns a folder in the data volume into a
//! callable extension, and the request shapes the host process serves.
//!
//! A runtime extension is `<dir>/<kind>/` with `manifest.json`,
//! `template.liquid`, and `build/<arch>/lib<kind>.so` next to the `meta.json`
//! that wrote it. The host checks an artifact before it calls it:
//!
//! | check | source |
//! |---|---|
//! | ABI version | the `.so` reports it |
//! | compiler version | the `.so` reports it |
//! | architecture | the `.so` reports it |
//! | artifact hash | `meta.json`, against the bytes on disk |
//!
//! A refusal names the reason, because a plugin that will not load has to be
//! something an operator can act on rather than a folder that did nothing.
//!
//! A loaded library is never unloaded. `dlclose` on Rust code with statics or
//! live tasks segfaults, so every library this process opens stays open until it
//! exits (ADR-0003). A reload swaps the live set and leaves the old libraries
//! mapped.

use crate::extensions::VarsRequest;
use blazonry_ext_api::{ABI_VERSION, ARCH, Meta, RUSTC_VERSION, VarsResponse, symbol};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// The directory a builder writes into, under the extension folder.
pub const BUILD_DIR: &str = "build";

/// One loaded extension, ready to answer vars.
pub struct Loaded {
    pub kind: String,
    pub manifest: Value,
    pub template: String,
    vars_fn: extern "C" fn(*const c_char) -> *mut c_char,
    free_fn: extern "C" fn(*mut c_char),
}

impl std::fmt::Debug for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loaded").field("kind", &self.kind).finish()
    }
}

/// Every library this process opened, held forever so a `Loaded` is never
/// unloaded out from under a caller.
fn graveyard() -> &'static Mutex<Vec<Arc<libloading::Library>>> {
    static LIBS: OnceLock<Mutex<Vec<Arc<libloading::Library>>>> = OnceLock::new();
    LIBS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Why one folder will not load. Every variant is a sentence for the operator.
#[derive(Debug)]
pub enum LoadError {
    NoBuild(PathBuf),
    NoMeta(String),
    HashMismatch,
    Abi(String),
    Open(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::NoBuild(p) => write!(f, "no build for this architecture: {0}", p.display()),
            LoadError::NoMeta(why) => write!(f, "build meta.json is missing or unreadable: {why}"),
            LoadError::HashMismatch => write!(
                f,
                "artifact hash does not match meta.json, so the file changed after it was built"
            ),
            LoadError::Abi(why) => write!(f, "extension will not load: {why}"),
            LoadError::Open(why) => write!(f, "extension failed to open: {why}"),
        }
    }
}

impl std::error::Error for LoadError {}

/// The `.so` this folder contributes for this architecture, or where it should
/// be.
pub fn artifact(folder: &Path) -> PathBuf {
    folder
        .join(BUILD_DIR)
        .join(ARCH)
        .join(format!("lib{}.so", folder_name(folder)))
}

fn folder_name(folder: &Path) -> String {
    folder
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Read and check one artifact, then load it.
pub fn load(folder: &Path) -> Result<Loaded, LoadError> {
    let so = artifact(folder);
    if !so.is_file() {
        return Err(LoadError::NoBuild(so));
    }
    let meta_path = so.with_extension("json");
    let meta_raw = std::fs::read_to_string(&meta_path)
        .map_err(|e| LoadError::NoMeta(format!("{}: {e}", meta_path.display())))?;
    let meta: BuildMeta = serde_json::from_str(&meta_raw)
        .map_err(|e| LoadError::NoMeta(format!("{}: {e}", meta_path.display())))?;
    if meta.sha256 != hex(sha256(&std::fs::read(&so).unwrap_or_default())) {
        return Err(LoadError::HashMismatch);
    }

    let lib = Arc::new(
        unsafe { libloading::Library::new(&so) }
            .map_err(|e| LoadError::Open(format!("{}: {e}", so.display())))?,
    );
    let reason = |why: String| LoadError::Abi(format!("{}: {why}", folder_name(folder)));
    let meta = read_meta(&lib).map_err(reason)?;
    if meta.abi != ABI_VERSION {
        return Err(reason(format!(
            "it speaks ABI {} and this host speaks ABI {ABI_VERSION}",
            meta.abi
        )));
    }
    if meta.arch != ARCH {
        return Err(reason(format!(
            "it was built for {} and this host is {ARCH}",
            meta.arch
        )));
    }
    if meta.rustc != RUSTC_VERSION {
        return Err(reason(format!(
            "it was built by {} and this host was built by {RUSTC_VERSION}",
            meta.rustc
        )));
    }
    let kind = meta
        .manifest
        .get("kind")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| reason("manifest.json has no kind".into()))?
        .to_string();
    let template = read_c_string(&lib, symbol::TEMPLATE)
        .map_err(reason)?
        .ok_or_else(|| reason("no template".into()))?;

    // The symbols are taken as plain function pointers, so nothing here holds a
    // borrow of the Library. The graveyard keeps the Library alive for as long
    // as any Loaded can be called.
    // SAFETY: the ABI contract says these two symbols exist with these
    // signatures, and the meta check above already rejected a foreign build.
    let vars_fn: extern "C" fn(*const c_char) -> *mut c_char =
        *unsafe { lib.get(symbol::VARS) }.map_err(|e| reason(e.to_string()))?;
    let free_fn: extern "C" fn(*mut c_char) =
        *unsafe { lib.get(symbol::FREE) }.map_err(|e| reason(e.to_string()))?;
    graveyard().lock().expect("library lock").push(lib.clone());

    Ok(Loaded {
        kind,
        manifest: meta.manifest,
        template,
        vars_fn,
        free_fn,
    })
}

impl Loaded {
    /// Vars for one request. The error text reaches the admin UI.
    pub fn vars(&self, req: &VarsRequest) -> Result<Value, String> {
        let body = serde_json::to_string(req).map_err(|e| format!("request: {e}"))?;
        let req_c = CString::new(body).map_err(|_| "request contains a NUL".to_string())?;
        let out = (self.vars_fn)(req_c.as_ptr());
        if out.is_null() {
            return Err("extension returned nothing".to_string());
        }
        let text = unsafe { CStr::from_ptr(out) }
            .to_str()
            .map_err(|e| format!("extension answer is not UTF-8: {e}"))?
            .to_string();
        (self.free_fn)(out);
        let answer: VarsResponse =
            serde_json::from_str(&text).map_err(|e| format!("extension answer: {e}"))?;
        match answer {
            VarsResponse::Vars(v) => Ok(v),
            VarsResponse::Error(e) => Err(e),
        }
    }
}

/// What a builder records beside the artifact.
#[derive(Debug, Deserialize)]
pub struct BuildMeta {
    pub sha256: String,
}

fn read_meta(lib: &libloading::Library) -> Result<Meta, String> {
    let raw = read_c_string(lib, symbol::META)?.ok_or("no meta")?;
    serde_json::from_str(&raw).map_err(|e| format!("meta is not readable: {e}"))
}

/// Calls a no-argument entry point that returns a C string, copies it, and hands
/// it back to the extension's own deallocator.
fn read_c_string(lib: &libloading::Library, name: &[u8]) -> Result<Option<String>, String> {
    // SAFETY: the ABI contract says these symbols exist with these signatures.
    let get: extern "C" fn() -> *mut c_char =
        *unsafe { lib.get(name) }.map_err(|e| format!("{name:?}: {e}"))?;
    let free: extern "C" fn(*mut c_char) =
        *unsafe { lib.get(symbol::FREE) }.map_err(|e| format!("{name:?}: {e}"))?;
    let p = get();
    if p.is_null() {
        return Ok(None);
    }
    let s = unsafe { CStr::from_ptr(p) }
        .to_str()
        .map_err(|e| format!("{name:?} is not UTF-8: {e}"))?
        .to_string();
    free(p);
    Ok(Some(s))
}

/// One scan result: what loaded, and what refused and why.
#[derive(Debug, Default)]
pub struct Scan {
    pub loaded: Vec<Arc<Loaded>>,
    /// `(folder, reason)` for every folder the host refused.
    pub refused: Vec<(String, String)>,
}

impl Scan {
    pub fn report(&self) -> Value {
        json!({
            "loaded": self.loaded.iter().map(|l| json!({
                "kind": l.kind,
                "label": l.manifest.get("label").cloned().unwrap_or(Value::Null),
            })).collect::<Vec<_>>(),
            "refused": self.refused.iter().map(|(folder, why)| json!({
                "folder": folder, "reason": why,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Load every extension folder in `dir`. A folder that will not load is
/// reported, never skipped quietly.
pub fn scan(dir: &Path) -> Scan {
    let mut out = Scan::default();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    let mut folders: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        // A dot folder is scratch space (.cargo, .git), not an extension.
        .filter(|p| !folder_name(p).starts_with('.'))
        .collect();
    folders.sort();
    for folder in folders {
        let name = folder_name(&folder);
        match load(&folder) {
            Ok(l) => out.loaded.push(Arc::new(l)),
            Err(e) => out.refused.push((name, e.to_string())),
        }
    }
    out
}

/// Build one extension folder and record its artifact. Returns the artifact
/// path.
///
/// An extension depends on `blazonry-ext-api` by version, because the folder has
/// to build wherever it lands. `ext_api` points cargo at the source with a
/// `[patch]`, which is what makes a folder with no registry access buildable.
pub fn build(folder: &Path, ext_api: Option<&Path>) -> anyhow::Result<PathBuf> {
    let manifest = folder.join("Cargo.toml");
    if !manifest.is_file() {
        anyhow::bail!("{} has no Cargo.toml", folder.display());
    }
    let target = folder.join(BUILD_DIR).join("cargo");
    let cargo = std::path::Path::new(env!("CARGO"));
    let mut cmd = std::process::Command::new(cargo);
    // Run from the folder: cargo reads config from the working directory up, and
    // the server's own config must not decide how a plugin is built.
    cmd.current_dir(folder);
    // Name the rustc beside this cargo rather than letting PATH pick one. ADR-0002
    // refuses a plugin built by a different compiler, so the build has to use the
    // same one the host reports.
    if let Some(rustc) = cargo
        .parent()
        .map(|d| d.join("rustc"))
        .filter(|p| p.is_file())
    {
        cmd.env("RUSTC", rustc);
    }
    cmd.arg("build")
        .arg("--release")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--target-dir")
        .arg(&target);
    if let Some(api) = ext_api {
        cmd.arg("--config").arg(format!(
            "patch.crates-io.blazonry-ext-api.path=\"{}\"",
            api.display()
        ));
    }
    let out = cmd.output()?;
    if !out.status.success() {
        let tail = String::from_utf8_lossy(&out.stderr).to_string();
        anyhow::bail!(tail);
    }
    let built = find_built_so(&target.join("release"))?;
    let dest_dir = folder.join(BUILD_DIR).join(ARCH);
    std::fs::create_dir_all(&dest_dir)?;
    let dest = artifact(folder);
    std::fs::copy(&built, &dest)?;
    let bytes = std::fs::read(&dest)?;
    let meta = json!({
        "sha256": hex(sha256(&bytes)),
        "arch": ARCH,
        "rustc": RUSTC_VERSION,
        "built_at": chrono::Utc::now().to_rfc3339(),
    });
    std::fs::write(dest.with_extension("json"), meta.to_string())?;
    Ok(dest)
}

fn find_built_so(release: &Path) -> anyhow::Result<PathBuf> {
    let mut sos: Vec<PathBuf> = std::fs::read_dir(release)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "so"))
        .collect();
    match sos.len() {
        1 => Ok(sos.pop().unwrap()),
        0 => anyhow::bail!("{} has no .so", release.display()),
        n => anyhow::bail!(
            "{} has {n} .so files; a cdylib crate must produce exactly one",
            release.display()
        ),
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hex(bytes: [u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The checks are only as good as the artifact they reject. A folder with no
    /// build, and a build whose bytes no longer match its recorded hash, must
    /// both refuse rather than load.
    #[test]
    fn a_folder_without_a_build_refuses_with_the_path_it_wanted() {
        let dir = std::env::temp_dir().join("blazonry-ext-host-empty-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("clock")).unwrap();
        let scan = scan(&dir);
        assert!(scan.loaded.is_empty());
        assert_eq!(scan.refused.len(), 1);
        assert!(scan.refused[0].1.contains("no build for this architecture"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_tampered_artifact_refuses_on_the_hash() {
        let dir = std::env::temp_dir().join("blazonry-ext-host-tampered-test");
        let _ = std::fs::remove_dir_all(&dir);
        let so = artifact(&dir.join("clock"));
        std::fs::create_dir_all(so.parent().unwrap()).unwrap();
        std::fs::write(&so, b"not really a shared library").unwrap();
        std::fs::write(
            so.with_extension("json"),
            json!({"sha256": "0".repeat(64)}).to_string(),
        )
        .unwrap();
        let err = load(&dir.join("clock")).unwrap_err();
        assert!(matches!(err, LoadError::HashMismatch), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_meta_refuses_before_the_library_opens() {
        let dir = std::env::temp_dir().join("blazonry-ext-host-nometa-test");
        let _ = std::fs::remove_dir_all(&dir);
        let so = artifact(&dir.join("clock"));
        std::fs::create_dir_all(so.parent().unwrap()).unwrap();
        std::fs::write(&so, b"still not a shared library").unwrap();
        let err = load(&dir.join("clock")).unwrap_err();
        assert!(matches!(err, LoadError::NoMeta(_)), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
