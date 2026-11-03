// SPDX-License-Identifier: AGPL-3.0-only
//! TRMNL Liquid compatibility test suite.
//!
//! Every test here encodes a specific, measured gap between blazonry's bare
//! liquid-rs stdlib parser and what the TRMNL community corpus actually uses
//! (measured against the public recipe corpus: `{% template %}` partials in
//! ~35% of recipes, ISO-8601 `date` passthrough corruption, `{% liquid %}`,
//! the `trmnl.*` context object, and a custom filter pack). Fixtures are
//! synthetic — small templates written to exercise exactly one gap each.

use serde_json::json;

mod common;
use common::render;

// ---------------------------------------------------------------------------
// Gap 1: `{% template %}` partials — TRMNL's partial-definition idiom.
// liquid-rs has render/include but no way to define a partial inline, and no
// `{% template %}` block at all (it's a TRMNL cloud Ruby extension).
// ---------------------------------------------------------------------------

#[test]
fn template_block_defines_a_partial_and_renders_nothing_inline() {
    let out = render(
        "{% template card %}CARD{% endtemplate %}before{% render \"card\" %}after",
        json!({}),
    )
    .unwrap();
    assert_eq!(out, "beforeCARDafter");
}

#[test]
fn template_partials_see_render_scope() {
    let out = render(
        "{% template greet %}Hi {{ name }}{% endtemplate %}{% render \"greet\", name: \"Bob\" %}",
        json!({}),
    )
    .unwrap();
    assert_eq!(out, "Hi Bob");
}

#[test]
fn template_partials_work_inside_loops() {
    let out = render(
        "{% template row %}[{{ item }}]{% endtemplate %}\
         {% for item in items %}{% render \"row\" %} {% endfor %}",
        json!({ "items": ["a", "b", "c"] }),
    )
    .unwrap();
    assert_eq!(out, "[a] [b] [c] ");
}

#[test]
fn multiple_template_blocks_all_register() {
    let out = render(
        "{% template a %}A{% endtemplate %}{% template b %}B{% endtemplate %}{% render \"b\" %}{{ \"\" }}{% render \"a\" %}",
        json!({}),
    )
    .unwrap();
    assert_eq!(out, "BA");
}

#[test]
fn whitespace_controlled_template_markers() {
    let out = render(
        "{%- template p -%}  X  {%- endtemplate -%}[{% render \"p\" %}]",
        json!({}),
    )
    .unwrap();
    assert_eq!(out, "[  X  ]");
}

#[test]
fn missing_partial_is_a_hard_error_not_silence() {
    // Define-the-error-out-of-existence: a render of an undefined partial must
    // fail loudly, never render blank on the device.
    let err = render("{% render \"nope\" %}", json!({})).unwrap_err();
    assert!(err.to_lowercase().contains("partial"), "got: {err}");
}

// ---------------------------------------------------------------------------
// Gap 2: strict `date` filter. liquid-rs's stdlib date silently echoes the
// input when it cannot parse it — with modern ISO 8601 (`Z`) inputs that means
// raw timestamp strings on the e-ink display, zero errors raised.
// ---------------------------------------------------------------------------

#[test]
fn date_parses_iso8601_utc_z() {
    let out = render(
        "{{ t | date: \"%Y-%m-%d %H:%M\" }}",
        json!({ "t": "2026-10-30T12:00:00Z" }),
    )
    .unwrap();
    assert_eq!(out, "2026-10-30 12:00");
}

#[test]
fn date_parses_iso8601_with_offset() {
    let out = render(
        "{{ t | date: \"%H:%M\" }}",
        json!({ "t": "2026-10-30T12:00:00+02:00" }),
    )
    .unwrap();
    assert_eq!(out, "12:00");
}

#[test]
fn date_parses_date_only() {
    let out = render(
        "{{ t | date: \"%B %-d, %Y\" }}",
        json!({ "t": "2026-10-30" }),
    )
    .unwrap();
    assert_eq!(out, "October 30, 2026");
}

#[test]
fn date_parses_space_separated_datetime() {
    let out = render(
        "{{ t | date: \"%Y-%m-%d\" }}",
        json!({ "t": "2026-10-30 07:45:00" }),
    )
    .unwrap();
    assert_eq!(out, "2026-10-30");
}

#[test]
fn date_parses_epoch_integer() {
    let out = render("{{ t | date: \"%Y-%m-%d\" }}", json!({ "t": 1761796800 })).unwrap();
    assert_eq!(out, "2025-10-30");
}

#[test]
fn date_parses_rfc2822() {
    let out = render(
        "{{ t | date: \"%Y-%m-%d\" }}",
        json!({ "t": "Fri, 30 Oct 2026 12:00:00 +0000" }),
    )
    .unwrap();
    assert_eq!(out, "2026-10-30");
}

#[test]
fn date_rejects_garbage_instead_of_echoing_it() {
    // The silent-passthrough bug, inverted: garbage must raise.
    let err = render("{{ t | date: \"%Y\" }}", json!({ "t": "not a date" })).unwrap_err();
    assert!(err.contains("date"), "got: {err}");
}

#[test]
fn date_supports_now_keyword() {
    let out = render("{{ \"now\" | date: \"%Y\" }}", json!({})).unwrap();
    assert_eq!(out, "2026");
}

// ---------------------------------------------------------------------------
// Gap 3: `{% liquid %}` line-oriented tag blocks (296 uses in the corpus;
// liquid-rs 0.26 cannot parse the tag at all).
// ---------------------------------------------------------------------------

#[test]
fn liquid_tag_assign_and_echo() {
    let out = render(
        "{% liquid\nassign city = \"Bellingham\"\necho city\n%}",
        json!({}),
    )
    .unwrap();
    assert_eq!(out, "Bellingham");
}

#[test]
fn liquid_tag_control_flow() {
    let out = render(
        "{% liquid\nfor i in (1..3)\nif i == 2\necho \"two\"\nelse\necho i\nendif\nendfor\n%}",
        json!({}),
    )
    .unwrap();
    assert_eq!(out, "1two3");
}

#[test]
fn liquid_tag_with_filters() {
    let out = render(
        "{% liquid\nassign s = \"hi\"\necho s | upcase\n%}",
        json!({}),
    )
    .unwrap();
    assert_eq!(out, "HI");
}

#[test]
fn echo_tag_standalone() {
    let out = render("{% assign x = 7 %}{% echo x | plus: 1 %}", json!({})).unwrap();
    assert_eq!(out, "8");
}

// ---------------------------------------------------------------------------
// Gap 4: the `trmnl.*` context object. Community templates dereference
// trmnl.user.utc_offset, trmnl.plugin_settings.*, trmnl.system.*, trmnl.device.*
// unconditionally; an absent namespace kills the whole render.
// ---------------------------------------------------------------------------

#[test]
fn render_screen_injects_trmnl_defaults() {
    // render_screen (not bare render) is the path that injects the namespace.
    let screen = blazonry::template::render_screen(
        r#"{"name":"t","parts":[{"type":"text","text":"{{ trmnl.user.utc_offset }}|{{ trmnl.device.width }}|{{ trmnl.user.locale }}"}]}"#,
        &json!({}),
    )
    .unwrap();
    let s = serde_json::to_string(&screen).unwrap();
    assert!(s.contains("0|800|en"), "got: {s}");
}

#[test]
fn caller_supplied_trmnl_values_win_over_defaults() {
    let out = render(
        "{{ trmnl.user.utc_offset }}/{{ trmnl.plugin_settings.instance_name }}",
        json!({ "trmnl": { "user": { "utc_offset": -480 },
                           "plugin_settings": { "instance_name": "Weather" } } }),
    )
    .unwrap();
    assert_eq!(out, "-480/Weather");
}

#[test]
fn trmnl_nil_chains_never_crash() {
    // Deep chains into unset keys must render empty, not error.
    let out = render(
        "[{{ trmnl.plugin_settings.custom_fields_values.nothing_here }}]",
        json!({}),
    )
    .unwrap();
    assert_eq!(out, "[]");
}

// ---------------------------------------------------------------------------
// Gap 5: the filter pack. Each of these was a verified `Unknown filter` parse
// failure against blazonry's bare stdlib parser.
// ---------------------------------------------------------------------------

#[test]
fn base64_encode() {
    let out = render("{{ s | base64_encode }}", json!({ "s": "hello" })).unwrap();
    assert_eq!(out, "aGVsbG8=");
}

#[test]
fn base64_decode_roundtrip() {
    let out = render("{{ s | base64_decode }}", json!({ "s": "aGVsbG8=" })).unwrap();
    assert_eq!(out, "hello");
}

#[test]
fn qr_code_emits_inline_svg() {
    let out = render("{{ s | qr_code }}", json!({ "s": "https://example.com" })).unwrap();
    assert!(out.contains("<svg"), "got: {}", &out[..40.min(out.len())]);
    assert!(out.contains("<rect") || out.contains("<path"));
}

#[test]
fn qr_code_accepts_scale_and_level_args() {
    let out = render("{{ s | qr_code: 8, \"h\" }}", json!({ "s": "x" })).unwrap();
    assert!(out.contains("<svg"));
}

#[test]
fn number_with_delimiter() {
    let out = render("{{ n | number_with_delimiter }}", json!({ "n": 1234567 })).unwrap();
    assert_eq!(out, "1,234,567");
    let out = render(
        "{{ n | number_with_delimiter: \".\" }}",
        json!({ "n": 1234567.89 }),
    )
    .unwrap();
    assert_eq!(out, "1.234.567.89");
}

#[test]
fn number_to_currency() {
    let out = render("{{ n | number_to_currency }}", json!({ "n": 1234.5 })).unwrap();
    assert_eq!(out, "$1,234.50");
}

#[test]
fn jsonify_and_parse_json_roundtrip() {
    let out = render("{{ data | jsonify }}", json!({ "data": { "a": 1 } })).unwrap();
    assert_eq!(out, "{\"a\":1}");
    let out = render("{{ s | parse_json | json }}", json!({ "s": "{\"a\":1}" })).unwrap();
    assert_eq!(out, "{\"a\":1}");
}

#[test]
fn parse_json_rejects_invalid_json() {
    let err = render("{{ s | parse_json }}", json!({ "s": "{oops" })).unwrap_err();
    assert!(err.contains("parse_json"), "got: {err}");
}

#[test]
fn parse_json_enables_property_access() {
    let out = render(
        "{% assign d = s | parse_json %}{{ d.b }}",
        json!({ "s": "{\"b\":42}" }),
    )
    .unwrap();
    assert_eq!(out, "42");
}

#[test]
fn sum_filter() {
    let out = render("{{ nums | sum }}", json!({ "nums": [1, 2, 3.5] })).unwrap();
    assert_eq!(out, "6.5");
    let out = render("{{ nums | sum }}", json!({ "nums": [10, 20] })).unwrap();
    assert_eq!(out, "30");
}

#[test]
fn where_exp_filter() {
    let out = render(
        "{% assign big = items | where_exp: \"i\", \"i.v > 2\" %}{{ big | map: \"n\" | join: \",\" }}",
        json!({ "items": [{ "n": "a", "v": 1 }, { "n": "b", "v": 3 }, { "n": "c", "v": 5 }] }),
    )
    .unwrap();
    assert_eq!(out, "b,c");
}

#[test]
fn where_exp_string_equality() {
    let out = render(
        "{% assign x = items | where_exp: \"i\", \"i.s == 'keep'\" %}{{ x | size }}",
        json!({ "items": [{ "s": "keep" }, { "s": "drop" }, { "s": "keep" }] }),
    )
    .unwrap();
    assert_eq!(out, "2");
}

#[test]
fn where_exp_comparisons() {
    let items =
        json!({ "items": [{ "n": "a", "v": 1 }, { "n": "b", "v": 2 }, { "n": "c", "v": 3 }] });
    let names = |cond: &str| {
        render(
            &format!("{{{{ items | where_exp: \"i\", \"{cond}\" | map: \"n\" | join: \",\" }}}}"),
            items.clone(),
        )
        .unwrap()
    };
    assert_eq!(names("i.v >= 2"), "b,c");
    assert_eq!(names("i.v <= 2"), "a,b");
    assert_eq!(names("i.v != 2"), "a,c");
    assert_eq!(names("i.v > 3"), "");
}

#[test]
fn pluralize_filter() {
    let out = render(
        "{{ n | pluralize: \"entry\", \"entries\" }}",
        json!({ "n": 1 }),
    )
    .unwrap();
    assert_eq!(out, "entry");
    let out = render(
        "{{ n | pluralize: \"entry\", \"entries\" }}",
        json!({ "n": 3 }),
    )
    .unwrap();
    assert_eq!(out, "entries");
}

#[test]
fn to_date_filter() {
    let out = render("{{ t | to_date }}", json!({ "t": "2026-10-30T23:59:00Z" })).unwrap();
    assert_eq!(out, "2026-10-30");
}

#[test]
fn l_date_accepts_locale_argument() {
    // TRMNL cloud localizes; blazonry applies the strftime core and accepts the
    // locale arg without failing (documented approximation).
    let out = render(
        "{{ t | l_date: \"%Y-%m-%d\", \"de\" }}",
        json!({ "t": "2026-10-30T12:00:00Z" }),
    )
    .unwrap();
    assert_eq!(out, "2026-10-30");
}

#[test]
fn date_with_local_time_shifts_by_offset() {
    let out = render(
        "{{ t | date_with_local_time: \"%H:%M\", -28800 }}",
        json!({ "t": "2026-10-30T12:00:00Z" }),
    )
    .unwrap();
    assert_eq!(out, "04:00");
}

#[test]
fn append_random_appends_hex_token() {
    let out = render("{{ u | append_random }}", json!({ "u": "img.png" })).unwrap();
    assert!(out.starts_with("img.png"), "got: {out}");
    let token = &out["img.png".len()..];
    assert!(!token.is_empty() && token.len() <= 8, "got token: {token}");
    assert!(
        token.chars().all(|c| c.is_ascii_hexdigit()),
        "got token: {token}"
    );
}

#[test]
fn sample_picks_from_array() {
    let out = render("{{ xs | sample }}", json!({ "xs": ["only"] })).unwrap();
    assert_eq!(out, "only");
}

#[test]
fn trim_and_lowercase_aliases() {
    let out = render("{{ s | trim }}", json!({ "s": "  pad  " })).unwrap();
    assert_eq!(out, "pad");
    let out = render("{{ s | lowercase }}", json!({ "s": "ABC" })).unwrap();
    assert_eq!(out, "abc");
}

// ---------------------------------------------------------------------------
// Regression guard: constructs already verified working on bare liquid-rs that
// the corpus leans on heavily — the compat layer must not break them.
// ---------------------------------------------------------------------------

#[test]
fn regression_negative_index_and_forloop() {
    let out = render(
        "{{ points[-1] }}|{% for p in points %}{{ forloop.index }}{{ p }}{% unless forloop.last %},{% endunless %}{% endfor %}",
        json!({ "points": [10, 20, 30] }),
    )
    .unwrap();
    assert_eq!(out, "30|110,220,330");
}

#[test]
fn regression_for_with_limit_offset_reversed() {
    let out = render(
        "{% for i in (1..5) limit: 2 offset: 1 %}{{ i }}{% endfor %}|{% for i in (1..3) reversed %}{{ i }}{% endfor %}",
        json!({}),
    )
    .unwrap();
    assert_eq!(out, "23|321");
}

#[test]
fn regression_default_increment_cycle_capture() {
    let out = render(
        "{% capture c %}{{ miss | default: \"dflt\" }}{% endcapture %}{{ c }}{% increment i %}{% increment i %}{% cycle \"a\", \"b\" %}",
        json!({}),
    )
    .unwrap();
    assert_eq!(out, "dflt01a");
}

#[test]
fn regression_raw_block_passes_through() {
    let out = render("{% raw %}{{ not_liquid }}{% endraw %}", json!({})).unwrap();
    assert_eq!(out, "{{ not_liquid }}");
}

// ---------------------------------------------------------------------------
// End-to-end: a realistic composite screen through render_screen.
// ---------------------------------------------------------------------------

#[test]
fn end_to_end_composite_screen() {
    let tmpl = r#"{"name":"agenda","parts":[{% for e in events %}{"type":"keyed_value","key":"{{ e.title }}","value":"{{ e.time | date: '%-l:%M%p' }}"}{% unless forloop.last %},{% endunless %}{% endfor %}]}"#;
    let screen = blazonry::template::render_screen(
        tmpl,
        &json!({
            "events": [
                { "title": "Feed Biscuit", "time": "2026-10-30T18:30:00Z" },
                { "title": "Yoga", "time": "2026-10-30T19:00:00Z" }
            ]
        }),
    )
    .unwrap();
    assert_eq!(screen.parts.len(), 2);
    let s = serde_json::to_string(&screen).unwrap();
    assert!(s.contains("Feed Biscuit"), "got: {s}");
    assert!(s.contains("6:30PM"), "got: {s}");
}
