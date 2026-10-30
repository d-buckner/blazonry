//! `{% render %}` with TRMNL cloud (Ruby Liquid) semantics.
//!
//! liquid-rs's stdlib `render` is *isolated*: the partial sees only the
//! variables passed to it. Ruby Liquid's `render` inherits the parent scope —
//! and TRMNL cloud templates rely on that constantly (partials reading loop
//! variables and globals without them being passed). This tag replaces the
//! stdlib one: same syntax, parent scope visible inside the partial.

use std::io::Write;

use liquid_core::error::{Result, ResultLiquidExt};
use liquid_core::model::Object;
use liquid_core::runtime::{GlobalFrame, Interrupt, InterruptRegister, StackFrame};
use liquid_core::{
    Expression, Language, ParseTag, Renderable, Runtime, TagReflection, TagTokenIter, Value,
    ValueView,
};
use liquid_lib::stdlib::ForloopObject;

#[derive(Copy, Clone, Debug, Default)]
pub struct RenderTag;

impl TagReflection for RenderTag {
    fn tag(&self) -> &str {
        "render"
    }
    fn description(&self) -> &str {
        "Insert the rendered content of a partial, inheriting the parent scope."
    }
}

impl ParseTag for RenderTag {
    fn parse(&self, mut arguments: TagTokenIter<'_>, _options: &Language) -> Result<Box<dyn Renderable>> {
        let partial = arguments
            .expect_next("Identifier or literal expected.")?
            .expect_value()
            .into_result()?;

        let mut vars: Vec<(String, Expression)> = Vec::new();
        let mut with_as: Option<(Expression, String)> = None;
        let mut for_as: Option<(Expression, String)> = None;

        match arguments.next().as_ref().map(|t| t.as_str()) {
            Some("with") => {
                let val = arguments
                    .expect_next("Value expected.")?
                    .expect_value()
                    .into_result()?;
                arguments
                    .expect_next("\"as\" expected.")?
                    .expect_str("as")
                    .into_result_custom_msg("\"as\" expected.")?;
                let name = arguments
                    .expect_next("Identifier expected.")?
                    .expect_identifier()
                    .into_result()?
                    .to_owned();
                with_as = Some((val, name));
                // optional trailing `, key: value` pairs
                if arguments.next().as_ref().map(|t| t.as_str()) == Some(",") {
                    parse_pairs(&mut arguments, &mut vars)?;
                }
            }
            Some("for") => {
                let val = arguments
                    .expect_next("Array expected.")?
                    .expect_value()
                    .into_result()?;
                arguments
                    .expect_next("\"as\" expected.")?
                    .expect_str("as")
                    .into_result_custom_msg("\"as\" expected.")?;
                let name = arguments
                    .expect_next("Identifier expected.")?
                    .expect_identifier()
                    .into_result()?
                    .to_owned();
                for_as = Some((val, name));
                if arguments.next().as_ref().map(|t| t.as_str()) == Some(",") {
                    parse_pairs(&mut arguments, &mut vars)?;
                }
            }
            Some(",") => parse_pairs(&mut arguments, &mut vars)?,
            _ => {}
        }

        arguments.expect_nothing()?;

        Ok(Box::new(Render { partial, vars, with_as, for_as }))
    }

    fn reflection(&self) -> &dyn TagReflection {
        self
    }
}

fn parse_pairs(arguments: &mut TagTokenIter<'_>, vars: &mut Vec<(String, Expression)>) -> Result<()> {
    let mut token = arguments.next();
    while let Some(t) = token {
        let id = t.expect_identifier().into_result()?.to_owned();
        arguments
            .expect_next("\":\" expected.")?
            .expect_str(":")
            .into_result_custom_msg("expected \":\"")?;
        let val = arguments
            .expect_next("expected value")?
            .expect_value()
            .into_result()?;
        vars.push((id.into(), val));
        token = match arguments.next() {
            Some(t) if t.as_str() == "," => arguments.next(),
            other => other,
        };
        if token.as_ref().is_some_and(|t| t.as_str() == ",") {
            token = arguments.next();
        }
    }
    Ok(())
}

#[derive(Debug)]
struct Render {
    partial: Expression,
    vars: Vec<(String, Expression)>,
    with_as: Option<(Expression, String)>,
    for_as: Option<(Expression, String)>,
}

impl Render {
    fn collect(&self, runtime: &dyn Runtime) -> Result<Object> {
        let mut root = Object::new();
        for (id, val) in &self.vars {
            let v = val
                .try_evaluate(runtime)
                .ok_or_else(|| liquid_core::Error::with_msg("failed to evaluate value"))?;
            root.insert(id.clone().into(), v.to_value());
        }
        if let Some((val, name)) = &self.with_as {
            let v = val
                .try_evaluate(runtime)
                .ok_or_else(|| liquid_core::Error::with_msg("failed to evaluate value"))?;
            root.insert(name.clone().into(), v.to_value());
        }
        Ok(root)
    }
}

impl Renderable for Render {
    fn render_to(&self, writer: &mut dyn Write, runtime: &dyn Runtime) -> Result<()> {
        let name = self
            .partial
            .evaluate(runtime)
            .trace_with(|| "{% render %}".to_owned().into())?
            .to_kstr()
            .into_owned();

        if let Some((val, var_name)) = &self.for_as {
            let evaluated = val
                .evaluate(runtime)
                .trace_with(|| "{% render for %}".to_owned().into())?;
            let array: Vec<Value> = evaluated
                .as_array()
                .map(|a| a.values().map(|v| v.to_value()).collect())
                .unwrap_or_default();
            let len = array.len();
            for (i, v) in array.into_iter().enumerate() {
                let mut root = self.collect(runtime)?;
                let forloop = ForloopObject::new(i, len);
                root.insert("forloop".into(), forloop.to_value());
                root.insert(var_name.clone().into(), v);
                let scope = GlobalFrame::new(StackFrame::new(runtime, &root));
                let partial = lookup(&scope, &name)?;
                partial
                    .render_to(writer, &scope)
                    .trace_with(|| format!("{{% render {name} %}}").into())
                    .context_key("index")
                    .value_with(|| format!("{}", i + 1).into())?;
                let current = scope.registers().get_mut::<InterruptRegister>().reset();
                if let Some(Interrupt::Break) = current {
                    break;
                }
            }
            return Ok(());
        }

        let root = self.collect(runtime)?;
        // Parent scope stays visible (Ruby semantics): StackFrame layers on top.
        let scope = StackFrame::new(runtime, &root);
        let partial = lookup(&scope, &name)?;
        partial
            .render_to(writer, &scope)
            .trace_with(|| format!("{{% render {name} %}}").into())
            .context_key_with(|| name.clone().into())
            .value_with(|| name.clone().into())
    }
}

fn lookup(runtime: &dyn Runtime, name: &str) -> Result<std::sync::Arc<dyn Renderable>> {
    runtime
        .partials()
        .get(name)
        .or_else(|_| runtime.partials().get(&format!("{name}.liquid")))
        .trace_with(|| format!("{{% render {name} %}}").into())
}
