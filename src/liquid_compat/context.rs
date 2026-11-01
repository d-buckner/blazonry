// SPDX-License-Identifier: AGPL-3.0-only
//! The `trmnl.*` context object community templates read:
//! `trmnl.user.utc_offset`, `trmnl.plugin_settings.instance_name`,
//! `trmnl.system.timestamp_utc`, `trmnl.user.locale`, `trmnl.device.width`, …
//!
//! Providers supply real values; anything absent gets safe defaults so an
//! imported template never dies on a nil chain.

use serde_json::{Map, Value};

/// Merge `vars` with a `trmnl` default context (caller's `trmnl` wins, deep-merged).
pub fn with_trmnl_context(vars: &Value) -> Value {
    let mut out = vars.clone();
    let Some(map) = out.as_object_mut() else {
        return Value::Object([("trmnl".to_string(), default_trmnl())].into_iter().collect());
    };
    match map.get_mut("trmnl") {
        Some(existing) => deep_merge(existing, &default_trmnl()),
        None => {
            map.insert("trmnl".to_string(), default_trmnl());
        }
    }
    out
}

fn default_trmnl() -> Value {
    serde_json::json!({
        "user": { "utc_offset": 0, "locale": "en", "time_zone_iana": "UTC" },
        "system": { "timestamp_utc": 0 },
        "device": { "width": crate::WIDTH, "height": crate::HEIGHT },
        "plugin_settings": { "instance_name": "", "custom_fields_values": {} }
    })
}

fn deep_merge(dst: &mut Value, src: &Value) {
    if let (Value::Object(d), Value::Object(s)) = (dst, src) {
        let s: Map<String, Value> = s.clone();
        for (k, v) in s {
            match d.get_mut(&k) {
                Some(dv) => deep_merge(dv, &v),
                None => {
                    d.insert(k, v);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn injects_defaults_and_preserves_caller() {
        let v = with_trmnl_context(&json!({"trmnl": {"user": {"utc_offset": -480}}, "x": 1}));
        assert_eq!(v["trmnl"]["user"]["utc_offset"], -480);
        assert_eq!(v["trmnl"]["user"]["locale"], "en");
        assert_eq!(v["x"], 1);
    }
}
