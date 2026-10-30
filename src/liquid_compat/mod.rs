//! TRMNL Liquid compatibility layer.
//!
//! mntr's authoring surface is Shopify Liquid (liquid-rs stdlib), but the
//! community corpus (TRMNL cloud, Ruby Liquid) leans on constructs liquid-rs
//! does not ship or ships with different semantics. This module closes the
//! measured gaps:
//!
//! - `{% template name %}…{% endtemplate %}`: TRMNL's partial-definition block.
//!   Preprocessed out of the source into the parser's partial store; renders
//!   nothing inline (matches TRMNL cloud semantics).
//! - `{% liquid %}`: line-oriented statement blocks, transpiled to `{% %}`.
//! - `{% render %}` with Ruby scope semantics: partials inherit the parent
//!   scope (liquid-rs's stdlib render is isolated; TRMNL templates rely on
//!   loop variables being visible inside partials).
//! - A lenient runtime: unresolved variables render empty (Ruby nil semantics)
//!   instead of aborting the whole screen (liquid-rs raises `Unknown variable`).
//! - A strict `date` filter: parses ISO 8601 (Z / offset / naive / date-only),
//!   epoch seconds, and RFC 2822, and *raises* on unparseable input instead of
//!   silently echoing it back (liquid-rs's silent passthrough renders raw
//!   timestamp strings on the device with zero errors).
//! - The TRMNL custom filter pack + Ruby-Liquid standard filters liquid-rs
//!   omits (see `filters`).
//! - The `trmnl.*` context object every community template reads.

mod context;
mod filters;
mod preprocess;
mod render_tag;
mod runtime;

pub use context::with_trmnl_context;

use std::sync::Arc;

use liquid_core::partials::PartialCompiler;
use liquid_core::parser::Language;
use liquid_core::runtime::{Renderable, RuntimeBuilder};
use liquid_lib::stdlib;

/// Build the full TRMNL-compat parse language: liquid-rs stdlib plus the
/// compat pack (custom `render`, filter pack, `echo`), overriding the stdlib
/// `date` and `render`.
fn language() -> Language {
    let mut lang = Language::empty();
    // stdlib blocks
    for b in [
        Box::new(stdlib::RawBlock) as Box<dyn liquid_core::ParseBlock>,
        stdlib::IfBlock.into(),
        stdlib::UnlessBlock.into(),
        stdlib::IfChangedBlock.into(),
        stdlib::ForBlock.into(),
        stdlib::TableRowBlock.into(),
        stdlib::CommentBlock.into(),
        stdlib::CaptureBlock.into(),
        stdlib::CaseBlock.into(),
    ] {
        lang.blocks.register(b.reflection().start_tag().to_owned(), b);
    }
    // stdlib tags (render replaced below)
    for t in [
        Box::new(stdlib::AssignTag) as Box<dyn liquid_core::ParseTag>,
        stdlib::BreakTag.into(),
        stdlib::ContinueTag.into(),
        stdlib::CycleTag.into(),
        stdlib::IncludeTag.into(),
        stdlib::IncrementTag.into(),
        stdlib::DecrementTag.into(),
    ] {
        lang.tags.register(t.reflection().tag().to_owned(), t);
    }
    lang.tags
        .register("render".into(), render_tag::RenderTag.into());
    lang.tags
        .register("echo".into(), filters::EchoTag.into());
    // stdlib filters
    for f in [
        Box::new(stdlib::Abs) as Box<dyn liquid_core::ParseFilter>,
        stdlib::Append.into(),
        stdlib::AtLeast.into(),
        stdlib::AtMost.into(),
        stdlib::Capitalize.into(),
        stdlib::Ceil.into(),
        stdlib::Compact.into(),
        stdlib::Concat.into(),
        stdlib::Default.into(),
        stdlib::DividedBy.into(),
        stdlib::Downcase.into(),
        stdlib::Escape.into(),
        stdlib::EscapeOnce.into(),
        stdlib::First.into(),
        stdlib::Floor.into(),
        stdlib::Join.into(),
        stdlib::Last.into(),
        stdlib::Lstrip.into(),
        stdlib::Map.into(),
        stdlib::Minus.into(),
        stdlib::Modulo.into(),
        stdlib::NewlineToBr.into(),
        stdlib::Plus.into(),
        stdlib::Prepend.into(),
        stdlib::Remove.into(),
        stdlib::RemoveFirst.into(),
        stdlib::Replace.into(),
        stdlib::ReplaceFirst.into(),
        stdlib::Reverse.into(),
        stdlib::Round.into(),
        stdlib::Rstrip.into(),
        stdlib::Size.into(),
        stdlib::Slice.into(),
        stdlib::Sort.into(),
        stdlib::SortNatural.into(),
        stdlib::Split.into(),
        stdlib::Strip.into(),
        stdlib::StripHtml.into(),
        stdlib::StripNewlines.into(),
        stdlib::Times.into(),
        stdlib::Truncate.into(),
        stdlib::TruncateWords.into(),
        stdlib::Uniq.into(),
        stdlib::Upcase.into(),
        stdlib::UrlDecode.into(),
        stdlib::UrlEncode.into(),
        stdlib::Where.into(),
    ] {
        lang.filters.register(f.reflection().name().to_owned(), f);
    }
    // compat pack (overrides stdlib `date`)
    filters::register_into(&mut lang);
    lang
}

/// Preprocess + parse + render a template. `{% template %}` blocks found in
/// `src` are extracted as partials and available to `{% render %}`/`{% include %}`.
pub fn render_str(src: &str, globals: &dyn liquid::ObjectView) -> anyhow::Result<String> {
    let (main, partials) = preprocess::extract_partials(src)?;
    let main = preprocess::transpile_liquid_tag(&main);

    let lang = Arc::new(language());

    let mut source = liquid_core::partials::InMemorySource::new();
    for (name, body) in &partials {
        source.add(name.clone(), body.clone());
    }
    let store = liquid_core::partials::EagerCompiler::new(source).compile(lang.clone())?;

    let elements = liquid_core::parser::parse(&main, &lang)?;
    let template = liquid_core::runtime::Template::new(elements);

    let base = RuntimeBuilder::new()
        .set_globals(globals)
        .set_partials(&*store)
        .build();
    let runtime = runtime::LenientRuntime::new(base);

    let mut out: Vec<u8> = Vec::new();
    template.render_to(&mut out, &runtime)?;
    Ok(String::from_utf8(out)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn r(src: &str) -> String {
        let g = liquid::to_object(&json!({})).unwrap();
        render_str(src, &g).unwrap()
    }

    #[test]
    fn liquid_tag_transpiles() {
        assert_eq!(r("{% liquid echo \"hi\" %}"), "hi");
    }

    #[test]
    fn template_block_defines_partial() {
        let out = r("{% template p %}P{% endtemplate %}A{% render \"p\" %}");
        assert_eq!(out, "AP");
    }
}
