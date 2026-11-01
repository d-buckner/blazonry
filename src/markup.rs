// SPDX-License-Identifier: AGPL-3.0-only
//! TRMNL markup front-end: HTML + Liquid -> Screen IR.
//!
//! The authoring surface is the same one TRMNL plugins use — real HTML with
//! the framework's class vocabulary (`title`, `value`, `label`, `flex--row`,
//! `gap--large`, `bg--black`, ...) — so the ~400-recipe public corpus is the
//! documentation and the test suite. Liquid renders first (partials, filters,
//! `trmnl.*` context all work); the rendered HTML is parsed with a strict
//! subset parser and mapped onto the Screen IR. No JSON is ever authored.
//!
//! Scope is deliberate: layout + text + design-system classes render natively;
//! elements that need the real framework (img, table, svg, progress-bar,
//! data-overflow) return `Unsupported` so `mntr ext check` can say "use the
//! browser backend" instead of rendering something wrong.

use crate::screen::{Align, Direction, Node, NodeKind, Screen, Style};

/// A markup construct the native renderer cannot express (yet).
#[derive(Debug)]
pub struct Unsupported(pub String);

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} requires the browser backend (not expressible natively)", self.0)
    }
}
impl std::error::Error for Unsupported {}

// ---------------------------------------------------------------------------
// Design system: class -> style, grounded in trmnl-framework CSS variables.
// ---------------------------------------------------------------------------

/// font sizes per text role: (base, small, xsmall, xxsmall, large, xlarge, xxlarge, xxxlarge, ...)
/// Values are the framework's `--*-font-size` px at scale 1.
fn role_font(role: &str, size: &str) -> Option<f32> {
    let px = |n: u32| n as f32;
    Some(match (role, size) {
        ("title", "") | ("title", "base") => px(21),
        ("title", "small") => px(16),
        ("title", "large") => px(30),
        ("title", "xlarge") => px(35),
        ("title", "xxlarge") => px(40),
        ("value", "") | ("value", "base") => px(38),
        ("value", "xxsmall") => px(16),
        ("value", "xsmall") => px(20),
        ("value", "small") => px(26),
        ("value", "large") => px(58),
        ("value", "xlarge") => px(74),
        ("value", "xxlarge") => px(96),
        ("value", "xxxlarge") => px(128),
        ("value", "mega") => px(170),
        ("value", "giga") => px(220),
        ("value", "tera") => px(290),
        ("value", "peta") => px(380),
        ("label", "") | ("label", "base") => px(16),
        ("label", "small") => px(12),
        ("label", "large") => px(21),
        ("label", "xlarge") => px(26),
        ("label", "xxlarge") => px(30),
        ("description", _) => match size {
            "" | "base" | "large" => px(16),
            "xlarge" => px(21),
            "xxlarge" => px(24),
            _ => return None,
        },
        ("meta", _) => px(12),
        _ => return None,
    })
}

const TEXT_ROLES: [&str; 5] = ["title", "value", "label", "description", "meta"];

fn gap_px(name: &str) -> Option<f32> {
    Some(match name {
        "none" => 0.0,
        "xsmall" => 5.0,
        "small" => 7.0,
        "base" => 10.0,
        "medium" => 16.0,
        "large" => 20.0,
        "xlarge" => 30.0,
        "xxlarge" => 40.0,
        _ => return None,
    })
}

/// True for responsive/variant prefixes we do not model (native = one viewport).
fn is_variant_prefixed(class: &str) -> bool {
    class.contains(':')
}

// ---------------------------------------------------------------------------
// HTML subset tokenizer
// ---------------------------------------------------------------------------

enum Token<'a> {
    Open { tag: &'a str, attrs: Vec<(&'a str, &'a str)>, self_closing: bool },
    Close { tag: &'a str },
    Text(&'a str),
}

fn flush_text<'s>(out: &mut Vec<Token<'s>>, src: &'s str, s: usize, e: usize) {
    if e > s {
        let t = &src[s..e];
        if !t.trim().is_empty() {
            out.push(Token::Text(t));
        }
    }
}

fn tokenize(src: &str) -> Vec<Token<'_>> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut text_start = 0usize;
    while i < b.len() {
        if b[i] == b'<' {
            flush_text(&mut out, src, text_start, i);
            // comments and doctype: skip to '>'
            if src[i..].starts_with("<!--") {
                i = src[i..].find("-->").map(|p| i + p + 3).unwrap_or(b.len());
                text_start = i;
                continue;
            }
            if src[i..].starts_with("<!") || src[i..].starts_with("<?") {
                i = src[i..].find('>').map(|p| i + p + 1).unwrap_or(b.len());
                text_start = i;
                continue;
            }
            let closing = b[i + 1] == b'/';
            let mut j = if closing { i + 2 } else { i + 1 };
            let tag_start = j;
            while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'>' && b[j] != b'/' {
                j += 1;
            }
            let tag = &src[tag_start..j];
            // attributes until '>' or '/>'
            let mut attrs = Vec::new();
            let mut self_closing = false;
            loop {
                while j < b.len() && (b[j] == b' ' || b[j] == b'\t' || b[j] == b'\n' || b[j] == b'\r') {
                    j += 1;
                }
                if j >= b.len() {
                    break;
                }
                if b[j] == b'>' {
                    j += 1;
                    break;
                }
                if b[j] == b'/' && b.get(j + 1) == Some(&b'>') {
                    self_closing = true;
                    j += 2;
                    break;
                }
                let an_start = j;
                while j < b.len() && b[j] != b'=' && b[j] != b'>' && !b[j].is_ascii_whitespace() && !(b[j] == b'/' && b.get(j + 1) == Some(&b'>')) {
                    j += 1;
                }
                let name = &src[an_start..j];
                if name.is_empty() {
                    j += 1;
                    continue;
                }
                if j < b.len() && b[j] == b'=' {
                    j += 1;
                    let quote = if b.get(j) == Some(&b'"') || b.get(j) == Some(&b'\'') {
                        let q = b[j];
                        j += 1;
                        Some(q)
                    } else {
                        None
                    };
                    let v_start = j;
                    if let Some(q) = quote {
                        while j < b.len() && b[j] != q {
                            j += 1;
                        }
                    } else {
                        while j < b.len() && b[j] != b'>' && !b[j].is_ascii_whitespace() {
                            j += 1;
                        }
                    }
                    attrs.push((name, &src[v_start..j]));
                    if quote.is_some() && j < b.len() {
                        j += 1;
                    }
                } else {
                    attrs.push((name, ""));
                }
            }
            out.push(if closing { Token::Close { tag } } else { Token::Open { tag, attrs, self_closing } });
            text_start = j;
            i = j;
        } else {
            i += 1;
        }
    }
    flush_text(&mut out, src, text_start, i);
    out
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            in_ws = true;
        } else {
            if in_ws && !out.is_empty() {
                out.push(' ');
            }
            in_ws = false;
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Element tree -> Screen IR
// ---------------------------------------------------------------------------

struct El<'a> {
    tag: &'a str,
    classes: Vec<&'a str>,
    children: Vec<El<'a>>,
    text: String,
}

impl<'a> El<'a> {
    fn has(&self, c: &str) -> bool {
        self.classes.iter().any(|x| *x == c)
    }
    fn class_starting_with(&self, p: &str) -> Option<&'a str> {
        self.classes.iter().find(|x| x.starts_with(p) && !is_variant_prefixed(x)).map(|x| &x[p.len()..])
    }
}

fn build_tree<'a>(tokens: Vec<Token<'a>>) -> Result<Vec<El<'a>>, Unsupported> {
    let mut root_children: Vec<El<'a>> = Vec::new();
    let mut stack: Vec<El<'a>> = Vec::new();
    for tok in tokens {
        match tok {
            Token::Open { tag, attrs, self_closing } => {
                let classes = attrs
                    .iter()
                    .find(|(k, _)| *k == "class")
                    .map(|(_, v)| v.split_whitespace().collect())
                    .unwrap_or_default();
                let el = El { tag, classes, children: Vec::new(), text: String::new() };
                if self_closing {
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(el),
                        None => root_children.push(el),
                    }
                } else {
                    stack.push(el);
                }
            }
            Token::Close { tag } => {
                // pop to matching tag (tolerate minor mismatches)
                if let Some(pos) = stack.iter().rposition(|e| e.tag == tag) {
                    let el = stack.remove(pos);
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(el),
                        None => root_children.push(el),
                    }
                }
            }
            Token::Text(t) => {
                let decoded = decode_entities(t);
                match stack.last_mut() {
                    Some(parent) => parent.text.push_str(&decoded),
                    None => {
                        let trimmed = decoded.trim();
                        if !trimmed.is_empty() {
                            root_children.push(El { tag: "#text", classes: vec![], children: vec![], text: trimmed.to_string() });
                        }
                    }
                }
            }
        }
    }
    // unclosed elements: close them in order
    while let Some(el) = stack.pop() {
        match stack.last_mut() {
            Some(parent) => parent.children.push(el),
            None => root_children.push(el),
        }
    }
    Ok(root_children)
}

/// Effective inherited style while descending (bg inversion flips fg).
#[derive(Clone, Copy)]
struct Inherited {
    fg: u8,
    bg: u8,
}

fn map_el(el: &El, inh: Inherited) -> Result<Option<Node>, Unsupported> {
    // elements the native renderer cannot express
    let unsupported = match el.tag {
        "img" => Some("img"),
        "table" | "tr" | "td" | "th" => Some("table"),
        "svg" | "canvas" | "iframe" | "video" | "audio" => Some(el.tag),
        "br" => None,
        "script" | "style" | "head" | "link" | "meta" | "title" => return Ok(None),
        _ => None,
    };
    if let Some(what) = unsupported {
        return Err(Unsupported(what.to_string()));
    }
    if el.has("progress-bar") {
        return Err(Unsupported("progress-bar".into()));
    }
    if el.has("hidden") {
        return Ok(None);
    }

    // text role (title/value/label/description/meta)
    let role = TEXT_ROLES.iter().find(|r| el.has(r)).copied();
    if let Some(role) = role {
        let size_suffix = el
            .classes
            .iter()
            .find_map(|c| c.strip_prefix(role).and_then(|rest| rest.strip_prefix("--")))
            .filter(|c| !is_variant_prefixed(c))
            .unwrap_or("");
        let font_px = role_font(role, size_suffix).unwrap_or_else(|| role_font(role, "").unwrap());
        // titles read bold on the panel (framework weight + our font pair)
        let style = Style { font_px, fg: inh.fg, bg: inh.bg, bold: role == "title" };
        let text = collapse_ws(&el.text);
        let mut node = Node { style, padding: [0.0; 4], gap: 0.0, align: Align::Start, grow: 0.0, width: None, height: None, border: false, kind: NodeKind::Text { text } };
        apply_common(el, &mut node);
        return Ok(Some(node));
    }

    // bare text node
    if el.tag == "#text" {
        let style = Style { fg: inh.fg, bg: inh.bg, ..Default::default() };
        let mut node = Node { style, padding: [0.0; 4], gap: 0.0, align: Align::Start, grow: 0.0, width: None, height: None, border: false, kind: NodeKind::Text { text: collapse_ws(&el.text) } };
        apply_common(el, &mut node);
        return Ok(Some(node));
    }

    // divider
    if el.tag == "hr" || el.has("divider") {
        let style = Style { fg: inh.fg, bg: inh.bg, ..Default::default() };
        let mut node = Node { style, padding: [0.0; 4], gap: 0.0, align: Align::Start, grow: 0.0, width: None, height: None, border: false, kind: NodeKind::Divider };
        apply_common(el, &mut node);
        return Ok(Some(node));
    }

    // container (div/span/flex/layout/grid/columns/column/title_bar/content/item)
    let mut style = Style { fg: inh.fg, bg: inh.bg, ..Default::default() };

    let direction = if el.has("flex--col") || el.has("layout--col") || el.has("column") {
        Direction::Column
    } else if el.has("flex--row") || el.has("layout--row") || el.has("columns") || el.class_starting_with("grid--cols-").is_some() {
        Direction::Row
    } else {
        Direction::Row
    };

    if let Some(cols) = el.class_starting_with("grid--cols-") {
        let _ = cols.parse::<usize>(); // grid children get grow below
    }
    let is_grid = el.has("grid");

    // bg inversion: bg--black paints black ground; text inside turns white
    let child_inh = if el.has("bg--black") {
        style.bg = 0;
        if style.fg == 0 { style.fg = 3; }
        Inherited { fg: 3, bg: 0 }
    } else if el.has("bg--gray") {
        style.bg = 2;
        Inherited { fg: inh.fg, bg: 2 }
    } else if el.has("bg--white") {
        style.bg = 3;
        Inherited { fg: 0, bg: 3 }
    } else {
        inh
    };

    let mut children = Vec::new();
    if !el.text.trim().is_empty() {
        let style = Style { fg: child_inh.fg, bg: child_inh.bg, ..Default::default() };
        children.push(Node { style, padding: [0.0; 4], gap: 0.0, align: Align::Start, grow: 0.0, width: None, height: None, border: false, kind: NodeKind::Text { text: collapse_ws(&el.text) } });
    }
    for child in &el.children {
        if let Some(mut n) = map_el(child, child_inh)? {
            if is_grid {
                n.grow = 1.0;
            }
            children.push(n);
        }
    }
    if children.is_empty() {
        return Ok(None);
    }
    let mut node = Node { style, padding: [0.0; 4], gap: 0.0, align: Align::Start, grow: 0.0, width: None, height: None, border: false, kind: NodeKind::Flex { direction, children } };
    apply_common(el, &mut node);
    Ok(Some(node))
}

/// Apply shared modifier classes to a node: gap, grow, align, colors, border.
fn apply_common(el: &El, node: &mut Node) {
    if let Some(g) = el.class_starting_with("gap--") {
        if let Some(px) = gap_px(g) {
            node.gap = px;
        }
    } else if el.has("gap") {
        node.gap = 10.0;
    }
    if el.has("grow") {
        node.grow = 1.0;
    }
    if el.has("flex--center") || el.has("flex--center-y") || el.has("align--center") || el.has("text--center") || el.has("col--center") {
        node.align = Align::Center;
    }
    if el.has("flex--right") || el.has("text--right") {
        node.align = Align::End;
    }
    if el.has("text--gray-75") || el.has("text--gray-50") {
        node.style.fg = 1;
    }
    if el.has("border--h-5") || el.has("border") {
        node.border = true;
    }
    if el.has("title_bar") {
        node.padding = [8.0, 16.0, 8.0, 16.0];
        node.gap = 10.0;
    }
}

/// Parse rendered TRMNL markup (HTML) into the Screen IR.
pub fn markup_to_screen(html: &str, name: &str) -> Result<Screen, Unsupported> {
    let tree = build_tree(tokenize(html))?;
    let inh = Inherited { fg: 0, bg: 3 };
    let mut parts = Vec::new();
    for el in tree {
        if let Some(n) = map_el(&el, inh)? {
            parts.push(n);
        }
    }
    Ok(Screen { name: name.to_string(), parts })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(s: &Screen) -> Vec<String> {
        s.parts.iter().map(|p| match &p.kind {
            NodeKind::Text { text } => format!("text:{text}"),
            NodeKind::Flex { direction, children } => format!("flex({:?}):{}", direction, children.len()),
            NodeKind::Divider => "divider".into(),
            _ => "other".into(),
        }).collect()
    }

    #[test]
    fn maps_title_and_flex_classes() {
        let html = r#"<div class="layout layout--col gap--small">
            <span class="title title--large">Hello</span>
            <span class="label label--small">meta</span>
        </div>"#;
        let s = markup_to_screen(html, "t").unwrap();
        assert_eq!(s.parts.len(), 1);
        let NodeKind::Flex { direction, children } = &s.parts[0].kind else { panic!() };
        assert_eq!(*direction, Direction::Column);
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].style.font_px, 30.0); // title--large
        assert_eq!(children[0].style.bold, true);
        assert_eq!(children[1].style.font_px, 12.0); // label--small
        assert_eq!(s.parts[0].gap, 7.0); // gap--small
    }

    #[test]
    fn bg_black_inverts_children() {
        let html = r#"<div class="flex flex--col bg--black"><span class="value value--xxlarge">72°</span></div>"#;
        let s = markup_to_screen(html, "t").unwrap();
        let NodeKind::Flex { children, .. } = &s.parts[0].kind else { panic!() };
        assert_eq!(s.parts[0].style.bg, 0);
        assert_eq!(children[0].style.fg, 3);
        assert_eq!(children[0].style.font_px, 96.0);
    }

    #[test]
    fn grid_children_grow() {
        let html = r#"<div class="grid grid--cols-3"><div class="col"><span class="label">a</span></div><div class="col"><span class="label">b</span></div></div>"#;
        let s = markup_to_screen(html, "t").unwrap();
        let NodeKind::Flex { children, .. } = &s.parts[0].kind else { panic!() };
        assert!(children.iter().all(|c| c.grow == 1.0));
    }

    #[test]
    fn img_is_unsupported() {
        let html = r#"<div class="layout"><img src="x.png"/></div>"#;
        let e = markup_to_screen(html, "t").unwrap_err();
        assert!(e.to_string().contains("browser backend"));
    }

    #[test]
    fn text_entities_and_whitespace_collapse() {
        let html = "<span class=\"title\">a &amp;   b\n  c</span>";
        let s = markup_to_screen(html, "t").unwrap();
        assert_eq!(kinds(&s), vec!["text:a & b c"]);
    }

    #[test]
    fn variant_prefixed_classes_ignored() {
        // lg: and portrait: variants must not change native (single-viewport) mapping
        let html = r#"<span class="title lg:title--xxlarge portrait:title--small">x</span>"#;
        let s = markup_to_screen(html, "t").unwrap();
        let NodeKind::Text { .. } = &s.parts[0].kind else { panic!() };
        assert_eq!(s.parts[0].style.font_px, 21.0); // base title
    }
}
