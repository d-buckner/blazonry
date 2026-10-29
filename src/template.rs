//! Liquid -> Screen: a Liquid template renders a JSON screen tree, which
//! deserializes into the screen vocabulary. Liquid stays the authoring surface
//! (TRMNL community muscle memory); JSON is the compile target the renderer eats.

use crate::screen::Screen;

pub fn render_screen(template_src: &str, vars: &serde_json::Value) -> anyhow::Result<Screen> {
    let parser = liquid::ParserBuilder::with_stdlib().build()?;
    let template = parser.parse(template_src)?;

    let globals = liquid::to_object(vars)?;
    let output = template.render(&globals)?;

    let screen: Screen = serde_json::from_str(&output)?;
    Ok(screen)
}
