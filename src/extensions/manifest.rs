// SPDX-License-Identifier: AGPL-3.0-only
//! Manifests: the metadata an extension folder ships next to its code.
//!
//! A folder declares what it is, what it consumes, and which settings it
//! takes. The framework reads that in three places: the admin API serves the
//! manifests so the UI builds its forms from them, [`ExtensionManifest::validate_settings`]
//! checks and completes the settings an operator submits, and the rebuild
//! dispatch reads `consumes` + `cardinality` to resolve providers.
//!
//! Two kinds ship a manifest:
//!
//! - `manifest.json` — the extension itself (settings it takes, shape it consumes).
//! - `<provider>.manifest.json` — a provider that lives in the same folder
//!   (config it takes, how often it should refresh).
//!
//! The split of validation duty is deliberate. Extension settings have no
//! hand-written checker, so the manifest is the whole contract for them.
//! Provider configs already validate in Rust (a CalDAV URL must parse, a
//! latitude must be in range), so a provider manifest carries UI metadata
//! only and its `config` fields describe the form, not the rules.
//!
//! A field is required unless it declares a `default` or `"optional": true`.
//! An absent field with a default is filled in; an optional field an operator
//! left blank stays absent, which is how "blank means derive it from the data"
//! reaches the extension code.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What one manifest field accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    String,
    Int,
    Number,
    Bool,
    Enum,
    /// `YYYY-MM-DD`.
    Date,
    /// One provider id publishing the shape this extension consumes.
    ProviderId,
    /// Any number of provider ids publishing that shape; empty means all.
    ProviderIds,
}

/// One settings or config field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Field {
    pub key: String,
    #[serde(rename = "type")]
    pub kind: FieldKind,
    pub label: String,
    /// Help line under the control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// Bounds for `int` and `number` fields. Whole numbers only: every bound
    /// in this repo is one, and an integer in the manifest stays an integer on
    /// the wire instead of arriving as `14.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
    /// A credential: the UI masks the input and never echoes it back.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub secret: bool,
}

impl Field {
    /// One field's own rule. Callers add the settings-path context.
    fn check(&self, value: &Value) -> anyhow::Result<()> {
        let bad = |why: String| anyhow::anyhow!("{why}");
        match self.kind {
            FieldKind::String => {
                if !value.is_string() {
                    return Err(bad("must be text".into()));
                }
            }
            FieldKind::Int => {
                let Some(n) = value.as_i64() else {
                    return Err(bad("must be a whole number".into()));
                };
                self.range(n as f64)?;
            }
            FieldKind::Number => {
                let Some(n) = value.as_f64() else {
                    return Err(bad("must be a number".into()));
                };
                self.range(n)?;
            }
            FieldKind::Bool => {
                if !value.is_boolean() {
                    return Err(bad("must be true or false".into()));
                }
            }
            FieldKind::Enum => {
                let Some(s) = value.as_str() else {
                    return Err(bad("must be one of a fixed list".into()));
                };
                if !self.values.iter().any(|v| v == s) {
                    return Err(bad(format!(
                        "must be one of {}",
                        self.values
                            .iter()
                            .map(|v| format!("\"{v}\""))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                }
            }
            FieldKind::Date => {
                let ok = value
                    .as_str()
                    .is_some_and(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok());
                if !ok {
                    return Err(bad("must be a date as YYYY-MM-DD".into()));
                }
            }
            FieldKind::ProviderId => {
                if value.as_i64().is_none_or(|id| id < 1) {
                    return Err(bad("must name one connected source".into()));
                }
            }
            FieldKind::ProviderIds => {
                if !value
                    .as_array()
                    .is_some_and(|a| a.iter().all(|v| v.as_i64().is_some_and(|id| id > 0)))
                {
                    return Err(bad("must be a list of connected sources".into()));
                }
            }
        }
        Ok(())
    }

    fn range(&self, n: f64) -> anyhow::Result<()> {
        if let Some(min) = self.min
            && n < min as f64
        {
            return Err(anyhow::anyhow!("must be at least {min}"));
        }
        if let Some(max) = self.max
            && n > max as f64
        {
            return Err(anyhow::anyhow!("must be at most {max}"));
        }
        Ok(())
    }

    /// True when an operator may leave this field out.
    fn skippable(&self) -> bool {
        self.optional || self.default.is_some()
    }
}

/// How many providers an extension needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cardinality {
    ExactlyOne,
    OneOrMore,
}

/// `manifest.json`: what an extension is and what it takes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionManifest {
    pub kind: String,
    pub label: String,
    pub blurb: String,
    /// The provider shape this extension consumes; null for a providerless one.
    pub consumes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cardinality: Option<Cardinality>,
    /// Appended to the error when no provider of the shape is connected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_hint: Option<String>,
    #[serde(default)]
    pub settings: Vec<Field>,
}

impl ExtensionManifest {
    /// Parse a bundled manifest. `path` names the file in the error, because a
    /// manifest that will not parse is a build-time bug, not a runtime one.
    pub fn parse(src: &str, path: &str) -> anyhow::Result<Self> {
        let m: Self = serde_json::from_str(src).map_err(|e| anyhow::anyhow!("{path}: {e}"))?;
        if m.kind.is_empty() {
            anyhow::bail!("{path}: kind is required");
        }
        if m.consumes.is_some() != m.cardinality.is_some() {
            anyhow::bail!(
                "{path}: consumes and cardinality must be set together ({} consumes {:?})",
                m.kind,
                m.cardinality
            );
        }
        Ok(m)
    }

    /// Check submitted settings and complete them with defaults.
    ///
    /// Unknown keys are rejected: a settings typo should fail the request, not
    /// sit in the row and silently do nothing.
    pub fn validate_settings(&self, settings: &Value) -> anyhow::Result<Value> {
        let given = settings
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("settings must be a JSON object for '{}'", self.kind))?;
        let mut out = serde_json::Map::new();
        for field in &self.settings {
            let Some(value) = given.get(&field.key) else {
                if let Some(default) = &field.default {
                    out.insert(field.key.clone(), default.clone());
                } else if !field.skippable() {
                    anyhow::bail!("settings.{} is required for '{}'", field.key, self.kind);
                }
                continue;
            };
            // A blank optional string means "derive it from the data", which is
            // the same thing an absent field means.
            if field.optional && value.as_str().is_some_and(str::is_empty) {
                continue;
            }
            if value.is_null() && field.skippable() {
                if let Some(default) = &field.default {
                    out.insert(field.key.clone(), default.clone());
                }
                continue;
            }
            field
                .check(value)
                .map_err(|e| anyhow::anyhow!("settings.{} {e}", field.key))?;
            out.insert(field.key.clone(), value.clone());
        }
        let known: Vec<&str> = self.settings.iter().map(|f| f.key.as_str()).collect();
        for key in given.keys() {
            if !known.contains(&key.as_str()) {
                anyhow::bail!(
                    "settings.{key} is not a setting of '{}' (known: {})",
                    self.kind,
                    known.join(", ")
                );
            }
        }
        Ok(Value::Object(out))
    }

    /// Complete stored settings with defaults. Rebuilds read stored rows, and
    /// a row written before a setting existed must still render. Values are
    /// left as stored: this fills gaps, it does not re-judge old data.
    pub fn with_defaults(&self, settings: &Value) -> Value {
        let mut out = settings.as_object().cloned().unwrap_or_default();
        for field in &self.settings {
            if !out.contains_key(&field.key)
                && let Some(default) = &field.default
            {
                out.insert(field.key.clone(), default.clone());
            }
        }
        Value::Object(out)
    }
}

/// `<provider>.manifest.json`: what a source is and how it is configured.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderManifest {
    pub kind: String,
    pub label: String,
    pub blurb: String,
    /// Refresh interval a new instance of this kind starts with, in seconds.
    pub default_refresh_secs: i64,
    #[serde(default)]
    pub config: Vec<Field>,
}

impl ProviderManifest {
    pub fn parse(src: &str, path: &str) -> anyhow::Result<Self> {
        serde_json::from_str(src).map_err(|e| anyhow::anyhow!("{path}: {e}"))
    }
}
