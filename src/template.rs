// SPDX-License-Identifier: AGPL-3.0-only
//! Liquid -> Screen: a Liquid template renders a JSON screen tree, which
//! deserializes into the screen vocabulary. Liquid stays the authoring surface
//! (TRMNL community muscle memory); JSON is the compile target the renderer eats.
//! Parsing goes through `crate::liquid_compat` (TRMNL compatibility layer).

use crate::liquid_compat;
use crate::screen::Screen;

/// Render a Liquid template to text through the full TRMNL compat pipeline
/// (partials, `{% liquid %}`, filter pack, `trmnl.*` context injection).
pub fn render_str(tmpl: &str, vars: &serde_json::Value) -> anyhow::Result<String> {
    let globals = liquid::to_object(&liquid_compat::with_trmnl_context(vars))?;
    liquid_compat::render_str(tmpl, &globals)
}

pub fn render_screen(template_src: &str, vars: &serde_json::Value) -> anyhow::Result<Screen> {
    let output = render_str(template_src, vars)?;

    let screen: Screen = serde_json::from_str(&output)?;
    Ok(screen)
}

#[cfg(test)]
mod tests {
    use super::render_screen;
    use serde_json::json;

    /// Liquid's loop metadata lives on `forloop`, not `for` (liquid-rs follows
    /// the Shopify tag vocabulary). `{% if for.last %}` silently evaluates nil
    /// as false, so a comma-joined array comes out `[1,2,]` and the JSON parse
    /// fails with a confusing "trailing comma".
    #[test]
    fn for_loop_joins_json_arrays() {
        let vars = json!({"week": [{"d": "M", "h": 3.2}, {"d": "T", "h": 5.1}]});
        let tmpl = r#"{"name":"c","parts":[{"type":"bars",
            "values": [{% for d in week %}{{ d.h }}{% unless forloop.last %},{% endunless %}{% endfor %}],
            "labels": [{% for d in week %}"{{ d.d }}"{% unless forloop.last %},{% endunless %}{% endfor %}]}]}"#;
        let screen = render_screen(tmpl, &vars).expect("loop template should compile");
        assert_eq!(screen.parts.len(), 1);
        match &screen.parts[0].kind {
            crate::screen::NodeKind::Bars { values, labels } => {
                assert_eq!(values, &[3.2, 5.1]);
                assert_eq!(labels, &["M".to_string(), "T".to_string()]);
            }
            other => panic!("expected bars, got {other:?}"),
        }
    }

    #[test]
    fn for_dot_last_is_nil_and_breaks_json() {
        let vars = json!({"week": [{"d": "M", "h": 3.2}, {"d": "T", "h": 5.1}]});
        let tmpl = r#"{"name":"c","parts":[{"type":"bars","values":[{% for d in week %}{{ d.h }}{% unless for.last %},{% endunless %}{% endfor %}]}]}"#;
        let err = render_screen(tmpl, &vars).unwrap_err().to_string();
        assert!(err.contains("trailing comma"), "got {err}");
    }
}
