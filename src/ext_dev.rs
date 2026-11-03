// SPDX-License-Identifier: AGPL-3.0-only
//! `blazonry ext` — the extension-authoring dev loop.
//!
//! An extension's authoring surface is a directory:
//!
//! ```text
//! ext/mything/
//!   template.liquid   # Liquid -> JSON screen tree
//!   vars.json         # fixture vars (the same shape the Rust half emits)
//! ```
//!
//! Three commands, all headless (no server, no DB, no device):
//!
//! - `blazonry ext new <name>`      scaffold `ext/<name>/` with a working template
//! - `blazonry ext check <path>`    validate: vars JSON, Liquid, screen schema,
//!   pixel render — machine-readable JSON output
//! - `blazonry ext render <path>`   write `<out>.bmp` (panel wire format) +
//!   `<out>.png` (eyeball preview)
//!
//! `check` is designed for agents: one command, exit code + JSON on stdout,
//! errors carry the stage that failed (`vars|liquid|json|schema|render`) so a
//! loop can branch on it. Humans get the same loop with a file watcher.

use std::path::{Path, PathBuf};

/// Resolved input paths for one extension directory (or bare template file).
struct ExtPaths {
    template: PathBuf,
    vars: PathBuf,
}

impl ExtPaths {
    fn resolve(arg: &Path, vars_override: Option<&Path>) -> anyhow::Result<Self> {
        let (template, vars) = if arg.is_dir() {
            (arg.join("template.liquid"), arg.join("vars.json"))
        } else {
            // bare template file; vars default to sibling vars.json
            let dir = arg.parent().unwrap_or(Path::new("."));
            (arg.to_path_buf(), dir.join("vars.json"))
        };
        let vars = vars_override.map(Path::to_path_buf).unwrap_or(vars);
        if !template.exists() {
            anyhow::bail!("template not found: {}", template.display());
        }
        Ok(Self { template, vars })
    }
}

/// Outcome of the full validation pipeline.
#[derive(Debug)]
pub struct CheckReport {
    pub ok: bool,
    /// Failed stage: "vars" | "liquid" | "json" | "schema" | "markup" | "render" (None if ok).
    pub stage: Option<&'static str>,
    pub error: Option<String>,
    pub parts: Option<usize>,
    /// Which front-end the template used: "screen-json" | "markup".
    pub front_end: Option<&'static str>,
}

impl CheckReport {
    fn ok(parts: usize, front_end: &'static str) -> Self {
        Self {
            ok: true,
            stage: None,
            error: None,
            parts: Some(parts),
            front_end: Some(front_end),
        }
    }
    fn fail(stage: &'static str, error: String) -> Self {
        Self {
            ok: false,
            stage: Some(stage),
            error: Some(error),
            parts: None,
            front_end: None,
        }
    }
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "ok": self.ok,
            "stage": self.stage,
            "error": self.error,
            "parts": self.parts,
            "front_end": self.front_end,
        })
    }
}

/// Run the whole pipeline: vars parse -> Liquid render -> front-end detect ->
/// Screen IR -> pixel render. Two front-ends, auto-detected on the rendered
/// output: TRMNL markup (HTML + design-system classes) or the JSON screen
/// tree (legacy/compat). Strict on vars (the runtime backend is lenient; the
/// dev loop is not — a typo'd fixture must fail loudly).
pub fn check(template_src: &str, vars_json: &str) -> CheckReport {
    let vars: serde_json::Value = match serde_json::from_str(vars_json) {
        Ok(v) => v,
        Err(e) => return CheckReport::fail("vars", format!("{e}")),
    };
    let output = match crate::template::render_str(template_src, &vars) {
        Ok(o) => o,
        Err(e) => return CheckReport::fail("liquid", format!("{e:#}")),
    };
    let trimmed = output.trim_start();
    let screen = if trimmed.starts_with('<') {
        // TRMNL markup front-end: HTML + design-system classes -> IR
        match crate::markup::markup_to_screen(&output, "check") {
            Ok(s) => s,
            Err(e) => return CheckReport::fail("markup", format!("{e}")),
        }
    } else {
        // JSON screen-tree front-end
        match serde_json::from_str::<crate::screen::Screen>(&output) {
            Ok(s) => s,
            Err(e) => {
                // serde can't distinguish "not JSON" from "wrong schema" — the
                // message can: unknown variant/field means the vocabulary rejected
                // it, everything else means Liquid emitted malformed JSON. Include
                // the rendered output either way so the author sees what Liquid did.
                let msg = e.to_string();
                let stage = if msg.contains("unknown variant") || msg.contains("unknown field") {
                    "schema"
                } else {
                    "json"
                };
                return CheckReport::fail(stage, format!("{msg}\nrendered output:\n{output}"));
            }
        }
    };
    // Pixel render catches font/layout failures the schema can't see.
    let renderer = match crate::render::Renderer::new() {
        Ok(r) => r,
        Err(e) => return CheckReport::fail("render", format!("{e:#}")),
    };
    let indices = renderer.render(&screen);
    let _ = crate::bmp::encode_2bit_bmp(crate::WIDTH, crate::HEIGHT, &indices);
    let front_end = if trimmed.starts_with('<') {
        "markup"
    } else {
        "screen-json"
    };
    CheckReport::ok(screen.parts.len(), front_end)
}

/// `blazonry ext check <path>` — validate an extension dir/template + vars.
pub fn cmd_check(path: &Path, vars_override: Option<&Path>) -> i32 {
    let paths = match ExtPaths::resolve(path, vars_override) {
        Ok(p) => p,
        Err(e) => {
            println!(
                "{}",
                serde_json::json!({"ok": false, "stage": "vars", "error": e.to_string()})
            );
            return 1;
        }
    };
    let template = std::fs::read_to_string(&paths.template).unwrap_or_default();
    let vars = match std::fs::read_to_string(&paths.vars) {
        Ok(v) => v,
        Err(e) => {
            println!(
                "{}",
                serde_json::json!({"ok": false, "stage": "vars", "error": format!("{}: {e}", paths.vars.display())})
            );
            return 1;
        }
    };
    let report = check(&template, &vars);
    println!("{}", report.to_json());
    if report.ok { 0 } else { 1 }
}

/// `blazonry ext render <path>` — write `<out>.bmp` + `<out>.png`.
pub fn cmd_render(path: &Path, vars_override: Option<&Path>, out: Option<&str>, bpp: u16) -> i32 {
    let paths = match ExtPaths::resolve(path, vars_override) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let template = match std::fs::read_to_string(&paths.template) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {}: {e}", paths.template.display());
            return 1;
        }
    };
    let vars_json = std::fs::read_to_string(&paths.vars).unwrap_or_else(|_| "{}".into());
    let vars: serde_json::Value = match serde_json::from_str(&vars_json) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: vars.json invalid: {e}");
            return 1;
        }
    };
    let rendered = match crate::template::render_str(&template, &vars) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e:#}");
            return 1;
        }
    };
    let screen = if rendered.trim_start().starts_with('<') {
        match crate::markup::markup_to_screen(&rendered, "render") {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    } else {
        match serde_json::from_str(&rendered) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: {e}\nrendered output:\n{rendered}");
                return 1;
            }
        }
    };
    let renderer = match crate::render::Renderer::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e:#}");
            return 1;
        }
    };
    let indices = renderer.render(&screen);
    let prefix = out.map(str::to_string).unwrap_or_else(|| {
        // prefer the extension dir name (ext/birthday -> birthday); bare
        // template files fall back to their own stem
        let dir_name = paths
            .template
            .parent()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()));
        match dir_name.as_deref() {
            Some(".") | None | Some("") => paths
                .template
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "screen".into()),
            Some(d) => d.to_string(),
        }
    });
    let bmp = match bpp {
        1 => crate::bmp::encode_1bit_bmp(crate::WIDTH, crate::HEIGHT, &indices),
        _ => crate::bmp::encode_2bit_bmp(crate::WIDTH, crate::HEIGHT, &indices),
    };
    let bmp_path = format!("{prefix}.bmp");
    let png_path = format!("{prefix}.png");
    std::fs::write(&bmp_path, &bmp).expect("write bmp");
    let mut img = image::GrayImage::new(crate::WIDTH as u32, crate::HEIGHT as u32);
    for (i, &v) in indices.iter().enumerate() {
        img.put_pixel(
            (i % crate::WIDTH) as u32,
            (i / crate::WIDTH) as u32,
            image::Luma([crate::bmp::LEVELS[v as usize]]),
        );
    }
    if let Err(e) = img.save(&png_path) {
        eprintln!("error: {png_path}: {e}");
        return 1;
    }
    println!(
        "wrote {bmp_path} ({bpp}bpp, {} bytes) + {png_path}",
        bmp.len()
    );
    0
}

const SCAFFOLD_TEMPLATE: &str = r##"{
  "name": "{{ title }}",
  "parts": [
    { "type": "text", "text": "{{ title }}", "style": { "font_px": 34, "bold": true }, "padding": [16, 20, 4, 20] },
    { "type": "divider", "padding": [4, 20, 8, 20] },
{% for row in rows %}
    { "type": "keyed_value", "key": "{{ row.key }}", "value": "{{ row.value }}", "padding": [6, 20, 6, 20], "style": { "font_px": 26 } }{% unless forloop.last %},{% endunless %}
{% endfor %}
{% if rows.size > 0 %},{% endif %}
    { "type": "text", "text": "{{ footer }}", "style": { "font_px": 15, "fg": 1 }, "padding": [12, 24, 0, 24] }
  ]
}
"##;

/// `blazonry ext new <name>` — scaffold `ext/<name>/` and verify it renders.
pub fn cmd_new(name: &str) -> i32 {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        eprintln!("error: name must be alphanumeric (dash/underscore ok), got '{name}'");
        return 1;
    }
    let dir = PathBuf::from("ext").join(name);
    if dir.exists() {
        eprintln!("error: {} already exists", dir.display());
        return 1;
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("error: {e}");
        return 1;
    }
    let vars = serde_json::json!({
        "title": name,
        "rows": [
            { "key": "First", "value": "hello" },
            { "key": "Second", "value": "world" }
        ],
        "footer": "edit template.liquid + vars.json, then: blazonry ext check"
    });
    std::fs::write(dir.join("template.liquid"), SCAFFOLD_TEMPLATE).expect("write template");
    std::fs::write(
        dir.join("vars.json"),
        format!("{}\n", serde_json::to_string_pretty(&vars).unwrap()),
    )
    .expect("write vars");
    println!("scaffolded {}/ (template.liquid, vars.json)", dir.display());
    // Prove the scaffold is green before handing it over.
    cmd_check(&dir, None)
}

/// CLI entry: `blazonry ext <new|check|render> ...`. Returns the process exit code.
pub fn cli(args: &[String]) -> i32 {
    let flag = |f: &str| -> Option<String> {
        args.iter()
            .position(|a| a == f)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    // drop the subcommand word from positionals
    let positional: Vec<&String> = if positional.first().map(|s| s.as_str()) == Some("ext") {
        positional[1..].to_vec()
    } else {
        positional
    };
    let sub = positional.first().map(|s| s.as_str()).unwrap_or("");
    let target = positional.get(1).map(Path::new);
    match sub {
        "new" => match target {
            Some(t) => cmd_new(&t.to_string_lossy()),
            None => {
                eprintln!("usage: blazonry ext new <name>");
                1
            }
        },
        "check" => match target {
            Some(t) => cmd_check(t, flag("--vars").as_deref().map(Path::new)),
            None => {
                eprintln!("usage: blazonry ext check <dir|template.liquid> [--vars vars.json]");
                1
            }
        },
        "render" => match target {
            Some(t) => {
                let bpp = flag("--bpp").and_then(|s| s.parse().ok()).unwrap_or(2u16);
                cmd_render(
                    t,
                    flag("--vars").as_deref().map(Path::new),
                    flag("--out").as_deref(),
                    bpp,
                )
            }
            None => {
                eprintln!(
                    "usage: blazonry ext render <dir|template.liquid> [--vars v.json] [--out prefix] [--bpp 1|2]"
                );
                1
            }
        },
        other => {
            eprintln!(
                "usage: blazonry ext new <name> | check <path> | render <path> [--vars v.json] [--out prefix] [--bpp 1|2]"
            );
            if !other.is_empty() {
                eprintln!("unknown ext command: {other}");
            }
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn check_accepts_good_template() {
        let tmpl =
            json!({"name": "t", "parts": [{"type": "text", "text": "{{ title }}"}]}).to_string();
        let r = check(&tmpl, &json!({"title": "hi"}).to_string());
        assert!(r.ok, "{:?}", r.error);
        assert_eq!(r.parts, Some(1));
    }

    #[test]
    fn check_flags_bad_vars_json() {
        let tmpl = "{}".to_string();
        let r = check(&tmpl, "{oops");
        assert_eq!(r.stage, Some("vars"));
    }

    #[test]
    fn check_flags_liquid_syntax_error() {
        let r = check("{% for x in %}", "{}");
        assert_eq!(r.stage, Some("liquid"));
    }

    /// The classic footgun: Liquid emitted valid text but invalid JSON
    /// (trailing comma from a nil loop var). `check` names the stage AND
    /// shows the rendered output.
    #[test]
    fn check_flags_trailing_comma_with_output() {
        let tmpl = r#"{"name":"c","parts":[{"type":"bars","values":[{% for d in week %}{{ d.h }}{% unless for.last %},{% endunless %}{% endfor %}]}]}"#;
        let vars = json!({"week": [{"h": 1}, {"h": 2}]}).to_string();
        let r = check(tmpl, &vars);
        assert_eq!(r.stage, Some("json"));
        assert!(r.error.as_deref().unwrap().contains("rendered output:"));
    }

    #[test]
    fn check_flags_schema_violation() {
        let tmpl = r#"{"name":"c","parts":[{"type":"nope"}]}"#;
        let r = check(tmpl, "{}");
        assert_eq!(r.stage, Some("schema"));
    }

    #[test]
    fn scaffold_template_itself_checks_green() {
        let vars = json!({
            "title": "x",
            "rows": [{"key": "a", "value": "b"}, {"key": "c", "value": "d"}],
            "footer": "f"
        });
        let r = check(SCAFFOLD_TEMPLATE, &vars.to_string());
        assert!(r.ok, "{:?}", r.error);
    }

    #[test]
    fn check_accepts_trmnl_markup_front_end() {
        let tmpl = r#"<div class="layout layout--col gap--small">
            <span class="title title--large">{{ title }}</span>
            {% for r in rows %}<span class="label label--small">{{ r.k }}: {{ r.v }}</span>{% endfor %}
        </div>"#;
        let vars =
            json!({"title": "T", "rows": [{"k": "a", "v": "1"}, {"k": "b", "v": "2"}]}).to_string();
        let r = check(tmpl, &vars);
        assert!(r.ok, "{:?}", r.error);
        assert_eq!(r.front_end, Some("markup"));
    }

    #[test]
    fn check_routes_markup_errors_to_markup_stage() {
        let tmpl = r#"<div class="layout"><img src="x.png"/></div>"#;
        let r = check(tmpl, "{}");
        assert_eq!(r.stage, Some("markup"));
        assert!(r.error.as_deref().unwrap().contains("browser backend"));
    }

    #[test]
    fn scaffold_template_survives_empty_rows() {
        let vars = json!({"title": "x", "rows": [], "footer": "f"});
        let r = check(SCAFFOLD_TEMPLATE, &vars.to_string());
        assert!(r.ok, "{:?}", r.error);
    }
}
