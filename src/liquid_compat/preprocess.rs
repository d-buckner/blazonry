// SPDX-License-Identifier: AGPL-3.0-only
//! Source-level preprocessing for constructs liquid-rs cannot parse:
//! `{% template %}` partial definitions and `{% liquid %}` statement blocks.

use regex_lite::Regex;

/// Split `{% template name %}…{% endtemplate %}` blocks out of the source.
/// Returns the main body (templates removed) and the named partial bodies.
/// TRMNL cloud semantics: a `{% template %}` block defines a partial and
/// renders nothing inline. Markers inside `{% raw %}` are left alone.
pub fn extract_partials(src: &str) -> anyhow::Result<(String, Vec<(String, String)>)> {
    let re = Regex::new(r"\{%-?\s*template\s+([A-Za-z0-9_\-]+)\s*-?%\}([\s\S]*?)\{%-?\s*endtemplate\s*-?%\}")
        .expect("valid regex");
    let mut out = String::with_capacity(src.len());
    let mut partials = Vec::new();
    let mut rest = src;
    loop {
        // The template regex runs only on non-raw segments; raw passes through verbatim.
        let (before, tail) = split_once_keep(rest, "{% raw %}");
        out.push_str(&String::from(re.replace_all(before, |c: &regex_lite::Captures| {
            partials.push((c[1].to_string(), c[2].to_string()));
            ""
        })));
        let Some(tail) = tail else { break };
        let (inner, after) = split_once_keep(tail, "{% endraw %}");
        out.push_str("{% raw %}");
        out.push_str(inner);
        match after {
            Some(after) => {
                out.push_str("{% endraw %}");
                rest = after;
            }
            // Unterminated raw: emit the rest verbatim; the parser will error.
            None => break,
        }
    }
    Ok((out, partials))
}

/// Transpile `{% liquid %}` blocks: each non-empty line becomes a `{% line %}`
/// statement (Shopify's line-oriented liquid tag).
pub fn transpile_liquid_tag(src: &str) -> String {
    let re = Regex::new(r"\{%-?\s*liquid\s+([\s\S]*?)-?%\}").expect("valid regex");
    re.replace_all(src, |c: &regex_lite::Captures| {
        let mut out = String::new();
        for line in c[1].lines() {
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            out.push_str("{% ");
            out.push_str(t);
            out.push_str(" %}");
        }
        out
    })
    .into_owned()
}

fn split_once_keep<'a>(hay: &'a str, needle: &str) -> (&'a str, Option<&'a str>) {
    match hay.find(needle) {
        Some(i) => (&hay[..i], Some(&hay[i + needle.len()..])),
        None => (hay, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_named_partials() {
        let (main, partials) = extract_partials("A{% template p %}B{% endtemplate %}C").unwrap();
        assert_eq!(main, "AC");
        assert_eq!(partials, vec![("p".to_string(), "B".to_string())]);
    }

    #[test]
    fn ignores_markers_inside_raw() {
        let (main, partials) =
            extract_partials("{% raw %}{% template x %}{% endtemplate %}{% endraw %}").unwrap();
        assert!(partials.is_empty());
        assert!(main.contains("{% template x %}"));
    }

    #[test]
    fn partials_outside_raw_still_extract() {
        let (main, partials) = extract_partials(
            "{% raw %}{% template x %}{% endraw %}A{% template p %}B{% endtemplate %}",
        )
        .unwrap();
        assert_eq!(partials, vec![("p".to_string(), "B".to_string())]);
        assert!(main.ends_with("A"));
    }

    #[test]
    fn liquid_tag_becomes_statements() {
        let t = transpile_liquid_tag("{% liquid\nassign x = 1\necho x\n%}");
        assert_eq!(t, "{% assign x = 1 %}{% echo x %}");
    }
}
