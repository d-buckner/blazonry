//! Shared test helper: render a Liquid template through mntr's full parser.
#![allow(dead_code)]

use serde_json::Value;

pub fn render(tmpl: &str, vars: Value) -> Result<String, String> {
    mntr::template::render_str(tmpl, &vars).map_err(|e| e.to_string())
}

pub fn render_ok(tmpl: &str, vars: Value) -> String {
    render(tmpl, vars).expect("template should render")
}

pub fn ok(tmpl: &str) -> String {
    render_ok(tmpl, serde_json::json!({}))
}
