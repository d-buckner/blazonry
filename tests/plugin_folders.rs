// SPDX-License-Identifier: AGPL-3.0-only
//! Invariants that hold a folder-based plugin layout together.
//!
//! Each built-in extension is a folder under `src/extensions/` holding its
//! manifest, template, fixture, and (for the ones that ship a source) its
//! provider and provider manifest. Nothing in the framework names a kind, so
//! the only thing stopping a half-added plugin is these checks: a folder that
//! is not registered, a manifest whose kind disagrees with its folder, or a
//! template that no longer renders would all ship silently.

use blazonry::extensions::{ExtensionManifest, Field, FieldKind, ProviderManifest};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/extensions")
}

/// Every immediate subfolder of `src/extensions/`.
fn folders() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(root())
        .expect("src/extensions/ missing")
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no extension folders found");
    out
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn extension_manifests() -> Vec<(String, PathBuf, ExtensionManifest)> {
    folders()
        .into_iter()
        .filter_map(|dir| {
            let mp = dir.join("manifest.json");
            if !mp.exists() {
                return None;
            }
            let m = ExtensionManifest::parse(&read(&mp), &mp.display().to_string())
                .expect("extension manifest must parse");
            Some((
                dir.file_name().unwrap().to_string_lossy().into_owned(),
                mp,
                m,
            ))
        })
        .collect()
}

/// Every `<provider>.manifest.json` shipped by any folder. The folder's own
/// `manifest.json` is excluded by the leading dot: it has no kind stem.
fn provider_manifests() -> Vec<(PathBuf, ProviderManifest)> {
    let mut out = Vec::new();
    for dir in folders() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect("read folder")
            .map(|e| e.expect("dir entry").path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().ends_with(".manifest.json"))
            })
            .collect();
        files.sort();
        for f in files {
            let m = ProviderManifest::parse(&read(&f), &f.display().to_string())
                .expect("provider manifest must parse");
            out.push((f, m));
        }
    }
    out
}

/// The whole pipeline on every bundled folder: manifest parses, the bundled
/// template renders against the bundled fixture, and the result renders to
/// pixels. Same code path as `blazonry ext check`.
#[test]
fn every_bundled_extension_checks_green() {
    for dir in folders() {
        if !dir.join("manifest.json").exists() {
            continue;
        }
        let template = read(&dir.join("template.liquid"));
        let vars = read(&dir.join("vars.json"));
        let report = blazonry::ext_dev::check(&template, &vars);
        assert!(
            report.ok,
            "{}: failed at {:?} — {}",
            dir.display(),
            report.stage,
            report.error.unwrap_or_default()
        );
    }
}

/// A folder is named for its kind, because the registry path and the admin UI
/// both key on the kind string.
#[test]
fn each_manifest_kind_matches_its_folder() {
    for (folder, mp, m) in extension_manifests() {
        assert_eq!(
            m.kind,
            folder,
            "{}: kind must match the folder",
            mp.display()
        );
        assert!(
            !m.label.trim().is_empty(),
            "{}: label is required",
            mp.display()
        );
        assert!(
            !m.blurb.trim().is_empty(),
            "{}: blurb is required",
            mp.display()
        );
    }
    for (f, m) in provider_manifests() {
        let stem = f
            .file_name()
            .unwrap()
            .to_string_lossy()
            .trim_end_matches(".manifest.json")
            .to_string();
        assert_eq!(
            m.kind,
            stem,
            "{}: kind must match the file name",
            f.display()
        );
    }
}

/// A folder without a registry entry is invisible; a registry entry without a
/// folder is a panic at startup. Both fail here.
#[test]
fn every_folder_is_registered_and_vice_versa() {
    let on_disk: BTreeSet<String> = extension_manifests()
        .into_iter()
        .map(|(_, _, m)| m.kind)
        .collect();
    let registered: BTreeSet<String> = blazonry::extensions::registry()
        .plugins
        .iter()
        .map(|p| p.manifest.kind.clone())
        .collect();
    assert_eq!(
        on_disk, registered,
        "folders and the extension registry disagree"
    );
    for p in &blazonry::extensions::registry().plugins {
        assert!(
            blazonry::extensions::lookup(&p.manifest.kind).is_some(),
            "{} not reachable through lookup",
            p.manifest.kind
        );
    }
}

/// A provider ships a manifest and a registry entry together, and the registry
/// is built from the folders rather than listed by hand.
#[test]
fn every_provider_manifest_is_registered() {
    let on_disk: BTreeSet<String> = provider_manifests()
        .into_iter()
        .map(|(_, m)| m.kind)
        .collect();
    let registered: BTreeSet<String> = blazonry::providers::registry()
        .keys()
        .map(|k| k.to_string())
        .collect();
    assert_eq!(
        on_disk, registered,
        "provider manifests and the provider registry disagree"
    );
    for (_, m) in provider_manifests() {
        assert!(
            m.default_refresh_secs >= 60,
            "{} refreshes faster than a minute",
            m.kind
        );
    }
}

/// `consumes` must name a shape some provider publishes, or the extension can
/// never be satisfied and its setup hint is a dead end.
#[test]
fn every_consumes_shape_has_a_provider() {
    let shapes: BTreeSet<String> = blazonry::providers::kind_metadata()
        .iter()
        .filter_map(|m| m["shape"].as_str().map(str::to_string))
        .collect();
    assert!(!shapes.is_empty(), "no provider publishes a shape");
    for (_, mp, m) in extension_manifests() {
        if let Some(shape) = &m.consumes {
            assert!(
                shapes.contains(shape),
                "{} consumes '{shape}', which no provider publishes (known: {:?})",
                mp.display(),
                shapes
            );
        }
    }
}

/// A provider picker on an extension that consumes nothing would be a control
/// with no effect, so the two must agree.
#[test]
fn provider_pickers_only_appear_on_extensions_that_consume_a_shape() {
    for (_, mp, m) in extension_manifests() {
        for f in &m.settings {
            let picker = matches!(f.kind, FieldKind::ProviderId | FieldKind::ProviderIds);
            assert!(
                !picker || m.consumes.is_some(),
                "{}: settings.{} picks a source but the extension consumes nothing",
                mp.display(),
                f.key
            );
        }
    }
}

/// Duplicate keys would make the admin form render two controls for one value,
/// and an empty label renders an unlabeled one.
#[test]
fn manifest_field_keys_are_unique_and_labeled() {
    for (_, mp, m) in extension_manifests() {
        let mut seen = BTreeSet::new();
        for f in &m.settings {
            assert!(
                seen.insert(f.key.clone()),
                "{}: duplicate key {}",
                mp.display(),
                f.key
            );
            assert!(
                !f.label.trim().is_empty(),
                "{}: {} has no label",
                mp.display(),
                f.key
            );
        }
    }
    for (f, m) in provider_manifests() {
        let mut seen = BTreeSet::new();
        for field in &m.config {
            assert!(
                seen.insert(field.key.clone()),
                "{}: duplicate key {}",
                f.display(),
                field.key
            );
            assert!(
                !field.label.trim().is_empty(),
                "{}: {} has no label",
                f.display(),
                field.key
            );
        }
    }
}

/// The admin UI renders a control per field type. An enum with no choices, or
/// a bounded number whose bounds are inverted, renders a control that can
/// never be satisfied.
#[test]
fn field_bounds_are_sane() {
    let all: Vec<(String, Vec<Field>)> = extension_manifests()
        .iter()
        .map(|(_, mp, m)| (mp.display().to_string(), m.settings.clone()))
        .chain(
            provider_manifests()
                .iter()
                .map(|(f, m)| (f.display().to_string(), m.config.clone())),
        )
        .collect();
    for (path, fields) in all {
        for f in fields {
            match f.kind {
                FieldKind::Enum => assert!(
                    !f.values.is_empty(),
                    "{path}: {} is an enum with no choices",
                    f.key
                ),
                FieldKind::Int | FieldKind::Number => {
                    if let (Some(lo), Some(hi)) = (f.min, f.max) {
                        assert!(lo <= hi, "{path}: {} has min {lo} above max {hi}", f.key);
                    }
                    if let Some(d) = &f.default {
                        let v = d.as_i64().or_else(|| d.as_f64().map(|x| x as i64));
                        if let (Some(v), Some(hi)) = (v, f.max) {
                            assert!(v <= hi, "{path}: {} default {v} exceeds max {hi}", f.key);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
