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
use serde::{Deserialize, Serialize};
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
    load_at(folder, &artifact(folder))
}

/// [`load`] for an artifact that is not the live one yet, which is how a check
/// runs a staged build without touching what the server sees.
pub fn load_at(folder: &Path, so: &Path) -> Result<Loaded, LoadError> {
    if !so.is_file() {
        return Err(LoadError::NoBuild(so.to_path_buf()));
    }
    let meta_path = so.with_extension("json");
    let meta_raw = std::fs::read_to_string(&meta_path)
        .map_err(|e| LoadError::NoMeta(format!("{}: {e}", meta_path.display())))?;
    let meta: BuildMeta = serde_json::from_str(&meta_raw)
        .map_err(|e| LoadError::NoMeta(format!("{}: {e}", meta_path.display())))?;
    if meta.sha256 != hex(sha256(&std::fs::read(so).unwrap_or_default())) {
        return Err(LoadError::HashMismatch);
    }

    let lib = Arc::new(
        unsafe { libloading::Library::new(so) }
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
    // Absolute paths, because the command runs with the folder as its working
    // directory and a relative path would resolve twice.
    let dir = folder.canonicalize()?;
    let folder: &Path = &dir;
    let ext_api = ext_api.map(|p| p.canonicalize()).transpose()?;
    let ext_api = ext_api.as_deref();
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
    let log = String::from_utf8_lossy(&out.stderr).to_string();
    let dir = staging(folder);
    if !out.status.success() {
        // A failed build leaves a record, so apply can name the failure instead
        // of reporting that nothing was ever built.
        let _ = write_record(
            &dir.join(BUILD_RECORD),
            &Record {
                ok: false,
                sha256: String::new(),
                at: now(),
                detail: tail_lines(&log),
            },
        );
        anyhow::bail!(log);
    }
    let built = find_built_so(&target.join("release"))?;
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(artifact_name(folder));
    std::fs::copy(&built, &dest)?;
    let bytes = std::fs::read(&dest)?;
    let sha = hex(sha256(&bytes));
    let meta = json!({
        "sha256": sha,
        "arch": ARCH,
        "rustc": RUSTC_VERSION,
        "built_at": now(),
    });
    std::fs::write(dest.with_extension("json"), meta.to_string())?;
    write_record(
        &dir.join(BUILD_RECORD),
        &Record {
            ok: true,
            sha256: sha,
            at: now(),
            detail: tail_lines(&log),
        },
    )?;
    Ok(dest)
}

// ---------------------------------------------------------------------------
// Generations and the apply gate (ADR-0003).
//
// A build lands in `build/<arch>/staging/`. Nothing there is live. `apply`
// promotes it, and moves the artifact it displaces into
// `build/<arch>/previous/` so `rollback` has something to put back.
// ---------------------------------------------------------------------------

/// The newest build, waiting. The loader never reads this directory.
pub fn staging(folder: &Path) -> PathBuf {
    folder.join(BUILD_DIR).join(ARCH).join("staging")
}

/// The artifact `apply` displaced, kept for `rollback`.
fn previous_dir(folder: &Path) -> PathBuf {
    folder.join(BUILD_DIR).join(ARCH).join("previous")
}

/// The file name one folder contributes for this architecture.
fn artifact_name(folder: &Path) -> String {
    format!("lib{}.so", folder_name(folder))
}

/// The name `apply` promotes to.
pub fn applied(folder: &Path) -> PathBuf {
    artifact(folder)
}

/// The name of the build record in the staging directory.
pub const BUILD_RECORD: &str = "build.json";
/// The name of the check record in the staging directory.
pub const CHECK_RECORD: &str = "check.json";

/// What the newest build or check found, and the artifact it found it on. The
/// hash is what makes a record expire: edit the source and the staged artifact
/// changes, so a green record from before the edit no longer describes anything.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub ok: bool,
    pub sha256: String,
    pub at: String,
    pub detail: String,
}

fn read_record(path: &Path) -> Option<Record> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

fn write_record(path: &Path, record: &Record) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(record)?)?;
    Ok(())
}

/// The last build and check for a folder, as the endpoints report them.
pub fn gate(folder: &Path) -> Value {
    let dir = staging(folder);
    json!({
        "staged": staging(folder).join(artifact_name(folder)).is_file(),
        "applied": applied(folder).is_file(),
        "rollback_available": previous_dir(folder).join(artifact_name(folder)).is_file(),
        "build": read_record(&dir.join(BUILD_RECORD)),
        "check": read_record(&dir.join(CHECK_RECORD)),
    })
}

/// The one rule apply enforces: the newest build and the newest check are both
/// green, and both describe the artifact staged right now.
fn gate_ok(folder: &Path) -> Result<String, String> {
    let staged = staging(folder).join(artifact_name(folder));
    let bytes =
        std::fs::read(&staged).map_err(|_| "nothing is staged; run a build first".to_string())?;
    let sha = hex(sha256(&bytes));
    let build = read_record(&staging(folder).join(BUILD_RECORD))
        .ok_or_else(|| "nothing has been built for this folder".to_string())?;
    if !build.ok {
        return Err(format!(
            "the last build failed: {}",
            one_line(&build.detail)
        ));
    }
    if build.sha256 != sha {
        return Err("the source changed after the last build; build again".to_string());
    }
    let check = read_record(&staging(folder).join(CHECK_RECORD))
        .ok_or_else(|| "this build has not been checked".to_string())?;
    if !check.ok {
        return Err(format!(
            "the last check failed: {}",
            one_line(&check.detail)
        ));
    }
    if check.sha256 != sha {
        return Err("the source changed after the last check; check again".to_string());
    }
    Ok(sha)
}

/// Promote the staged artifact. The live one moves to `previous/`, both its
/// metadata file included, because the loader hashes the `.so` against that
/// record and a new file beside an old record is a refusal.
pub fn apply(folder: &Path) -> anyhow::Result<Value> {
    let sha = gate_ok(folder).map_err(|why| anyhow::anyhow!("apply refused: {why}"))?;
    let live = applied(folder);
    let prev = previous_dir(folder);
    if live.is_file() {
        std::fs::create_dir_all(&prev)?;
        std::fs::rename(&live, prev.join(artifact_name(folder)))?;
        std::fs::rename(
            live.with_extension("json"),
            prev.join(artifact_name(folder)).with_extension("json"),
        )?;
    }
    let stage = staging(folder);
    let name = artifact_name(folder);
    std::fs::rename(stage.join(&name), &live)?;
    std::fs::rename(
        stage.join(&name).with_extension("json"),
        live.with_extension("json"),
    )?;
    Ok(json!({
        "applied": true,
        "sha256": sha,
        "rolled_back_to": null,
        "rollback_available": true,
    }))
}

/// Put the displaced artifact back. The one being displaced goes to staging with
/// its records, so a rollback is reversible and re-applying it is byte-identical
/// to the artifact that was checked. Rollback is not a way to push an unchecked
/// artifact: nothing here skips the gate.
pub fn rollback(folder: &Path) -> anyhow::Result<Value> {
    let prev = previous_dir(folder);
    let name = artifact_name(folder);
    let back = prev.join(&name);
    if !back.is_file() {
        anyhow::bail!("{} holds nothing to roll back to", prev.display());
    }
    let live = applied(folder);
    let stage = staging(folder);
    if live.is_file() {
        std::fs::create_dir_all(&stage)?;
        std::fs::rename(&live, stage.join(&name))?;
        std::fs::rename(
            live.with_extension("json"),
            stage.join(&name).with_extension("json"),
        )?;
    }
    std::fs::rename(&back, &live)?;
    std::fs::rename(
        prev.join(&name).with_extension("json"),
        live.with_extension("json"),
    )?;
    Ok(json!({
        "rolled_back_to": live.display().to_string(),
        "rollback_available": false,
    }))
}

/// Run the staged artifact the way the host will: load it, ask it for vars against
/// the folder's `request.json`, and paint the template it reports. Green means a
/// screen came out. Whether the author likes the result is not something a host
/// can decide.
///
/// A failed check still writes its record. Without that, apply would find the
/// previous green record and promote an artifact that just failed.
pub fn check(folder: &Path) -> anyhow::Result<Value> {
    let staged = staging(folder).join(artifact_name(folder));
    let sha = sha256_hex(&staged).unwrap_or_default();
    let outcome = check_run(folder, &staged);
    let (ok, detail, mut value) = match outcome {
        Ok(v) => (true, String::new(), v),
        Err(e) => (false, e.to_string(), Value::Null),
    };
    write_record(
        &staging(folder).join(CHECK_RECORD),
        &Record {
            ok,
            sha256: sha,
            at: now(),
            detail: detail.clone(),
        },
    )?;
    if !ok {
        anyhow::bail!(detail);
    }
    if let Some(map) = value.as_object_mut() {
        map.insert("ok".to_string(), Value::Bool(true));
    }
    Ok(value)
}

fn check_run(folder: &Path, staged: &Path) -> anyhow::Result<Value> {
    let loaded = load_at(folder, staged).map_err(|e| anyhow::anyhow!("{e}"))?;
    let req = fixture_request(folder, &loaded.kind)?;
    let vars = loaded.vars(&req).map_err(anyhow::Error::msg)?;
    let indices = crate::render::paint_indices(&loaded.template, &vars)?;
    let bmp = crate::bmp::encode_2bit_bmp(crate::WIDTH, crate::HEIGHT, &indices);
    let bmp_path = staged.with_extension("bmp");
    let png_path = staged.with_extension("png");
    std::fs::write(&bmp_path, &bmp)?;
    let mut img = image::GrayImage::new(crate::WIDTH as u32, crate::HEIGHT as u32);
    for (i, &v) in indices.iter().enumerate() {
        img.put_pixel(
            (i % crate::WIDTH) as u32,
            (i / crate::WIDTH) as u32,
            image::Luma([crate::bmp::LEVELS[v as usize]]),
        );
    }
    img.save(&png_path)?;
    // The PNG travels inline, because the caller is an author looking at the
    // result through an API and a path on this machine tells it nothing. This is
    // the admin plane, not the device plane: /api/display sends an image_url
    // because the panel firmware cannot parse a 60KB string (see api.rs).
    let png = std::fs::read(&png_path)?;
    Ok(json!({
        "kind": loaded.kind,
        "vars": vars,
        "bmp": bmp_path.display().to_string(),
        "png": png_path.display().to_string(),
        "png_base64": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &png),
        "png_bytes": png.len(),
    }))
}

/// The request a check runs on: the folder's `request.json` if it has one, so the
/// author chooses the settings and the snapshots worth testing.
fn fixture_request(folder: &Path, kind: &str) -> anyhow::Result<VarsRequest> {
    let path = folder.join("request.json");
    if path.is_file() {
        let raw = std::fs::read_to_string(&path)?;
        return Ok(serde_json::from_str(&raw)?);
    }
    Ok(VarsRequest {
        kind: kind.to_string(),
        settings: json!({}),
        local_now: now(),
        provider_ids: vec![],
        snapshots: vec![],
    })
}

/// The folder one kind lives in. The host owns the volume, so a kind arriving over
/// the socket has to name a plain directory and nothing else.
pub fn folder_for(dir: &Path, kind: &str) -> anyhow::Result<PathBuf> {
    folder_in(dir, kind, false)
}

/// [`folder_for`], with the one difference `write` needs: a new kind has no folder
/// yet, so it makes one.
pub fn folder_in(dir: &Path, kind: &str, create: bool) -> anyhow::Result<PathBuf> {
    if kind.is_empty()
        || kind == "."
        || kind == ".."
        || kind.contains('/')
        || kind.contains('\'')
        || kind.starts_with('.')
    {
        anyhow::bail!("'{kind}' is not an extension folder name");
    }
    let folder = dir.join(kind);
    if create {
        std::fs::create_dir_all(&folder)?;
        return Ok(folder);
    }
    if !folder.is_dir() {
        anyhow::bail!("no extension folder named '{kind}' under {}", dir.display());
    }
    Ok(folder)
}

/// Write authored files into a folder. Paths inside the folder stay inside it.
pub fn write_files(folder: &Path, files: &std::collections::BTreeMap<String, String>) -> Value {
    let mut written: Vec<String> = Vec::new();
    for (rel, text) in files {
        let path = match safe_inside(folder, rel) {
            Ok(p) => p,
            Err(e) => return json!({"ok": false, "error": e.to_string()}),
        };
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return json!({"ok": false, "error": e.to_string()});
        }
        if let Err(e) = std::fs::write(&path, text) {
            return json!({"ok": false, "error": format!("{}: {e}", path.display())});
        }
        written.push(rel.clone());
    }
    json!({"ok": true, "written": written})
}

/// `folder/rel`, refusing anything that escapes the folder.
pub fn safe_inside(folder: &Path, rel: &str) -> anyhow::Result<PathBuf> {
    if rel.starts_with('/') || rel.starts_with('\\') {
        anyhow::bail!("'{rel}' is an absolute path; name a file inside the folder");
    }
    let mut out = folder.to_path_buf();
    for part in rel.split(['/', '\\']) {
        match part {
            "" | "." => continue,
            ".." => anyhow::bail!("'{rel}' leaves the extension folder"),
            p if p.starts_with('-') => anyhow::bail!("'{rel}' is not a file name"),
            p => out.push(p),
        }
    }
    if out == folder {
        anyhow::bail!("'{rel}' names the folder itself");
    }
    Ok(out)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// The last lines of a build log: enough to act on, small enough to return.
fn tail_lines(log: &str) -> String {
    log.lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(20)
        .collect::<Vec<_>>()
        .join("\n")
}

fn one_line(text: &str) -> String {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no detail")
        .trim()
        .to_string()
}

fn sha256_hex(path: &Path) -> anyhow::Result<String> {
    Ok(hex(sha256(&std::fs::read(path)?)))
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
    /// A staged artifact and a green build record for it. The gate reads bytes and
    /// records, so a test can stage bytes that are not a real library.
    fn stage(folder: &Path, bytes: &[u8]) -> String {
        let dir = staging(folder);
        std::fs::create_dir_all(&dir).unwrap();
        let so = dir.join(artifact_name(folder));
        std::fs::write(&so, bytes).unwrap();
        std::fs::write(so.with_extension("json"), "{}").unwrap();
        let sha = hex(sha256(bytes));
        write_record(
            &dir.join(BUILD_RECORD),
            &Record {
                ok: true,
                sha256: sha.clone(),
                at: now(),
                detail: String::new(),
            },
        )
        .unwrap();
        sha
    }

    fn check_green(folder: &Path, sha: &str) {
        write_record(
            &staging(folder).join(CHECK_RECORD),
            &Record {
                ok: true,
                sha256: sha.to_string(),
                at: now(),
                detail: String::new(),
            },
        )
        .unwrap();
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn apply_refuses_when_nothing_is_staged() {
        let folder = temp("blazonry-gate-empty-test").join("clock");
        std::fs::create_dir_all(&folder).unwrap();
        let err = apply(&folder).unwrap_err().to_string();
        assert!(err.contains("nothing is staged"), "{err}");
    }

    #[test]
    fn apply_refuses_a_failed_build() {
        let folder = temp("blazonry-gate-buildred-test").join("clock");
        std::fs::create_dir_all(&folder).unwrap();
        stage(&folder, b"artifact bytes");
        write_record(
            &staging(&folder).join(BUILD_RECORD),
            &Record {
                ok: false,
                sha256: String::new(),
                at: now(),
                detail: "error[E0432]: unresolved import\n".to_string(),
            },
        )
        .unwrap();
        let err = apply(&folder).unwrap_err().to_string();
        assert!(err.contains("last build failed"), "{err}");
    }

    #[test]
    fn apply_refuses_when_the_source_changed_after_the_check() {
        // The whole point of binding a record to a hash: edit the source after a
        // green check and the check no longer describes anything.
        let folder = temp("blazonry-gate-stale-test").join("clock");
        std::fs::create_dir_all(&folder).unwrap();
        let sha = stage(&folder, b"artifact bytes");
        check_green(&folder, &sha);
        assert!(gate_ok(&folder).is_ok());
        // A rebuild lands, so the build record matches again and only the check
        // has gone stale.
        stage(&folder, b"edited source, new artifact");
        let err = apply(&folder).unwrap_err().to_string();
        assert!(err.contains("changed after the last check"), "{err}");
        // A build record for different bytes than the ones on disk is the other
        // half: the author edited between build and check.
        write_record(
            &staging(&folder).join(BUILD_RECORD),
            &Record {
                ok: true,
                sha256: "0".repeat(64),
                at: now(),
                detail: String::new(),
            },
        )
        .unwrap();
        let err = apply(&folder).unwrap_err().to_string();
        assert!(err.contains("changed after the last build"), "{err}");
    }

    #[test]
    fn apply_refuses_a_build_that_was_never_checked() {
        let folder = temp("blazonry-gate-nocheck-test").join("clock");
        std::fs::create_dir_all(&folder).unwrap();
        stage(&folder, b"artifact bytes");
        let err = apply(&folder).unwrap_err().to_string();
        assert!(err.contains("has not been checked"), "{err}");
    }

    #[test]
    fn a_green_pair_applies_and_rollback_puts_the_old_one_back() {
        let folder = temp("blazonry-gate-roundtrip-test").join("clock");
        std::fs::create_dir_all(&folder).unwrap();
        let first = stage(&folder, b"first");
        check_green(&folder, &first);
        apply(&folder).unwrap();
        assert_eq!(std::fs::read(artifact(&folder)).unwrap(), b"first");

        let second = stage(&folder, b"second");
        check_green(&folder, &second);
        apply(&folder).unwrap();
        assert_eq!(std::fs::read(artifact(&folder)).unwrap(), b"second");

        rollback(&folder).unwrap();
        assert_eq!(std::fs::read(artifact(&folder)).unwrap(), b"first");
        // The generation rollback displaced stays staged with its records, so
        // re-applying it is allowed and puts back exactly what was checked. Each
        // apply displaces one artifact, so apply and rollback ping-pong.
        apply(&folder).unwrap();
        assert_eq!(std::fs::read(artifact(&folder)).unwrap(), b"second");
        rollback(&folder).unwrap();
        assert_eq!(std::fs::read(artifact(&folder)).unwrap(), b"first");
        // Two is the depth: nothing older is kept.
        assert!(
            rollback(&folder)
                .unwrap_err()
                .to_string()
                .contains("nothing to roll back")
        );
    }

    #[test]
    fn write_refuses_a_path_that_leaves_the_folder() {
        let dir = temp("blazonry-gate-write-test");
        for bad in ["../../etc/passwd", "src/../../x", "/etc/passwd"] {
            assert!(
                safe_inside(&dir, bad).is_err(),
                "{bad} should not resolve inside the folder"
            );
        }
        assert!(safe_inside(&dir, "src/lib.rs").is_ok());
    }

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
