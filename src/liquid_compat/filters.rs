// SPDX-License-Identifier: AGPL-3.0-only
//! The TRMNL filter pack: Ruby-Liquid standard filters liquid-rs omits, plus
//! TRMNL cloud's custom filters, plus a strict `date` that replaces liquid-rs's
//! silent-passthrough one.
//!
//! Approximations are deliberate and documented per filter: parse-level
//! acceptance is what unblocks imports; where exact cloud semantics need data
//! mntr does not model (locales, i18n dictionaries), the filter accepts the
//! arguments and renders the locale-independent core.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::Write;

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, TimeZone, Utc};
use liquid_core::error::ResultLiquidExt;
use liquid_core::{
    Display_filter, Error, Filter, FilterParameters, FilterReflection, FromFilterParameters,
    ParseFilter, ParseTag, Result, TagReflection, Value, ValueView,
};
use liquid_core::{Language, Renderable, Runtime, TagTokenIter};

// ---------------------------------------------------------------------------
// date (strict): replaces the stdlib filter, which silently echoes the input
// back when it cannot parse it — that renders raw ISO strings on the device.
// ---------------------------------------------------------------------------

/// Strict timestamp parsing: ISO 8601 (Z / offset / naive), date-only,
/// space-separated, RFC 2822, epoch seconds (numeric or numeric string),
/// and the `now` / `today` keywords.
pub fn parse_dt(input: &dyn ValueView) -> Option<DateTime<FixedOffset>> {
    if let Some(scalar) = input.as_scalar() {
        if let Some(n) = scalar.to_float().or_else(|| scalar.to_integer().map(|i| i as f64)) {
            if scalar.to_kstr().chars().all(|c| c.is_ascii_digit() || c == '-' || c == '.') {
                return Utc.timestamp_opt(n as i64, 0).single().map(Into::into);
            }
        }
        let s = scalar.to_kstr().into_owned();
        match s.trim().to_ascii_lowercase().as_str() {
            "now" | "today" => return Some(Utc::now().into()),
            _ => {}
        }
        if let Ok(d) = DateTime::parse_from_rfc3339(&s) {
            return Some(d);
        }
        // ISO with a space instead of T, with or without offset
        for fmt in [
            "%Y-%m-%dT%H:%M:%S%.f",
            "%Y-%m-%d %H:%M:%S%.f",
            "%Y-%m-%dT%H:%M",
            "%Y-%m-%d %H:%M",
        ] {
            if let Ok(d) = NaiveDateTime::parse_from_str(&s, fmt) {
                return Some(d.and_utc().into());
            }
        }
        if let Ok(d) = NaiveDate::parse_from_str(&s, "%Y-%m-%d") {
            return Some(d.and_hms_opt(0, 0, 0).unwrap().and_utc().into());
        }
        if let Ok(d) = DateTime::parse_from_rfc2822(&s) {
            return Some(d);
        }
        if let Ok(n) = s.trim().parse::<i64>() {
            return Utc.timestamp_opt(n, 0).single().map(Into::into);
        }
        None
    } else {
        None
    }
}

fn strftime(dt: &DateTime<FixedOffset>, format: &str) -> Result<String, Error> {
    // Ruby's %s is epoch seconds; chrono's strftime does not support it, so
    // substitute the value first, then format the rest.
    // chrono's Display-on-format panics on invalid specifiers; pre-validate
    // with StrftimeItems (yields Item::Error) and fail as Liquid instead of
    // crashing the render loop.
    use chrono::format::{Item, StrftimeItems};
    let owned;
    let format = if format.contains("%s") {
        owned = format.replace("%s", &dt.timestamp().to_string());
        owned.as_str()
    } else {
        format
    };
    if StrftimeItems::new(format).any(|item| matches!(item, Item::Error)) {
        return Err(Error::with_msg(format!("date: invalid strftime format `{format}`")));
    }
    Ok(dt.format(format).to_string())
}

fn date_error(input: &dyn ValueView) -> Error {
    Error::with_msg("date: unparseable datetime")
        .context("input", input.to_kstr().into_owned())
}

#[derive(Debug, FilterParameters)]
struct DateArgs {
    #[parameter(description = "strftime format string.", arg_type = "str")]
    format: liquid_core::Expression,
}

#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(
    name = "date",
    description = "Formats a datetime (strict: raises on unparseable input).",
    parameters(DateArgs),
    parsed(DateFilter)
)]
pub struct Date;

#[derive(Debug, FromFilterParameters, Display_filter)]
#[name = "date"]
struct DateFilter {
    #[parameters]
    args: DateArgs,
}

impl Filter for DateFilter {
    fn evaluate(&self, input: &dyn ValueView, runtime: &dyn Runtime) -> Result<Value> {
        let args = self.args.evaluate(runtime)?;
        let dt = parse_dt(input).ok_or_else(|| date_error(input))?;
        Ok(Value::scalar(strftime(&dt, args.format.as_str())?))
    }
}

// ---------------------------------------------------------------------------
// l_date / date_with_local_time: locale-aware in TRMNL cloud; mntr renders the
// locale-independent strftime core and accepts (ignores) the locale argument.
// ---------------------------------------------------------------------------

#[derive(Debug, FilterParameters)]
struct LDateArgs {
    #[parameter(description = "strftime format.", arg_type = "str")]
    format: liquid_core::Expression,
    #[parameter(description = "BCP-47 locale (accepted, not applied).", arg_type = "str")]
    locale: Option<liquid_core::Expression>,
}

#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(
    name = "l_date",
    description = "Localized date; mntr applies the strftime format only.",
    parameters(LDateArgs),
    parsed(LDateFilter)
)]
pub struct LDate;

#[derive(Debug, FromFilterParameters, Display_filter)]
#[name = "l_date"]
struct LDateFilter {
    #[parameters]
    args: LDateArgs,
}

impl Filter for LDateFilter {
    fn evaluate(&self, input: &dyn ValueView, runtime: &dyn Runtime) -> Result<Value> {
        let args = self.args.evaluate(runtime)?;
        let dt = parse_dt(input).ok_or_else(|| date_error(input))?;
        Ok(Value::scalar(strftime(&dt, args.format.as_str())?))
    }
}

#[derive(Debug, FilterParameters)]
struct LocalTimeArgs {
    #[parameter(description = "strftime format.", arg_type = "str")]
    format: liquid_core::Expression,
    #[parameter(description = "UTC offset in seconds.", arg_type = "float")]
    offset: Option<liquid_core::Expression>,
}

#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(
    name = "date_with_local_time",
    description = "Formats a datetime shifted by a UTC offset.",
    parameters(LocalTimeArgs),
    parsed(LocalTimeFilter)
)]
pub struct DateWithLocalTime;

#[derive(Debug, FromFilterParameters, Display_filter)]
#[name = "date_with_local_time"]
struct LocalTimeFilter {
    #[parameters]
    args: LocalTimeArgs,
}

impl Filter for LocalTimeFilter {
    fn evaluate(&self, input: &dyn ValueView, runtime: &dyn Runtime) -> Result<Value> {
        let args = self.args.evaluate(runtime)?;
        let dt = parse_dt(input).ok_or_else(|| date_error(input))?;
        let offset = args.offset.unwrap_or(0.0);
        let offset = FixedOffset::east_opt(offset as i32).unwrap_or_else(|| FixedOffset::east_opt(0).unwrap());
        Ok(Value::scalar(strftime(&dt.with_timezone(&offset), args.format.as_str())?))
    }
}

#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(name = "to_date", description = "Parses a datetime to a date (strict).", parsed(ToDateFilter))]
pub struct ToDate;

#[derive(Debug, Default, Display_filter)]
#[name = "to_date"]
struct ToDateFilter;

impl Filter for ToDateFilter {
    fn evaluate(&self, input: &dyn ValueView, _runtime: &dyn Runtime) -> Result<Value> {
        let dt = parse_dt(input).ok_or_else(|| date_error(input))?;
        Ok(Value::scalar(dt.format("%Y-%m-%d").to_string()))
    }
}

// ---------------------------------------------------------------------------
// JSON round-trip
// ---------------------------------------------------------------------------

fn to_json_string(input: &dyn ValueView) -> String {
    let value = input.to_value();
    serde_json::to_string(&value).unwrap_or_else(|_| input.to_kstr().to_string())
}

#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(name = "jsonify", description = "Serializes the input as compact JSON.", parsed(JsonifyFilter))]
pub struct Jsonify;

#[derive(Debug, Default, Display_filter)]
#[name = "jsonify"]
struct JsonifyFilter;

impl Filter for JsonifyFilter {
    fn evaluate(&self, input: &dyn ValueView, _runtime: &dyn Runtime) -> Result<Value> {
        Ok(Value::scalar(to_json_string(input)))
    }
}

/// TRMNL cloud alias for jsonify.
#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(name = "json", description = "Serializes the input as compact JSON.", parsed(JsonFilter))]
pub struct JsonFilterParse;

#[derive(Debug, Default, Display_filter)]
#[name = "json"]
struct JsonFilter;

impl Filter for JsonFilter {
    fn evaluate(&self, input: &dyn ValueView, _runtime: &dyn Runtime) -> Result<Value> {
        Ok(Value::scalar(to_json_string(input)))
    }
}

#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(name = "parse_json", description = "Parses a JSON string into data.", parsed(ParseJsonFilter))]
pub struct ParseJson;

#[derive(Debug, Default, Display_filter)]
#[name = "parse_json"]
struct ParseJsonFilter;

impl Filter for ParseJsonFilter {
    fn evaluate(&self, input: &dyn ValueView, _runtime: &dyn Runtime) -> Result<Value> {
        let s = input.to_kstr();
        let parsed: serde_json::Value = serde_json::from_str(s.as_str())
            .map_err(|e| Error::with_msg(format!("parse_json: invalid JSON ({e})")))?;
        let value: Value = serde_json::from_str(&parsed.to_string())
            .map_err(|e| Error::with_msg(format!("parse_json: {e}")))?;
        Ok(value)
    }
}

// ---------------------------------------------------------------------------
// Numeric formatting (Ruby ActiveSupport)
// ---------------------------------------------------------------------------

fn group_digits(int_part: &str, delim: &str) -> String {
    let negative = int_part.starts_with('-');
    let digits = int_part.trim_start_matches('-');
    let mut out = String::new();
    for (i, c) in digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push_str(delim);
        }
        out.push(c);
    }
    let out: String = out.chars().rev().collect();
    if negative { format!("-{out}") } else { out }
}

#[derive(Debug, FilterParameters)]
struct DelimiterArgs {
    #[parameter(description = "Grouping delimiter.", arg_type = "str")]
    delimiter: Option<liquid_core::Expression>,
}

#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(
    name = "number_with_delimiter",
    description = "Groups digits with a delimiter (default comma).",
    parameters(DelimiterArgs),
    parsed(DelimiterFilter)
)]
pub struct NumberWithDelimiter;

#[derive(Debug, FromFilterParameters, Display_filter)]
#[name = "number_with_delimiter"]
struct DelimiterFilter {
    #[parameters]
    args: DelimiterArgs,
}

impl Filter for DelimiterFilter {
    fn evaluate(&self, input: &dyn ValueView, runtime: &dyn Runtime) -> Result<Value> {
        let args = self.args.evaluate(runtime)?;
        let delim = args
            .delimiter
            .map(|v| v.to_kstr().to_string())
            .unwrap_or_else(|| ",".to_string());
        let s = input.to_kstr();
        let (int_part, frac) = match s.split_once('.') {
            Some((a, b)) => (a.to_string(), format!(".{b}")),
            None => (s.to_string(), String::new()),
        };
        Ok(Value::scalar(format!("{}{}", group_digits(&int_part, &delim), frac)))
    }
}

#[derive(Debug, FilterParameters)]
struct CurrencyArgs {
    #[parameter(description = "Currency unit.", arg_type = "str")]
    unit: Option<liquid_core::Expression>,
}

#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(
    name = "number_to_currency",
    description = "Formats a number as currency (default $, 2 decimals).",
    parameters(CurrencyArgs),
    parsed(CurrencyFilter)
)]
pub struct NumberToCurrency;

#[derive(Debug, FromFilterParameters, Display_filter)]
#[name = "number_to_currency"]
struct CurrencyFilter {
    #[parameters]
    args: CurrencyArgs,
}

impl Filter for CurrencyFilter {
    fn evaluate(&self, input: &dyn ValueView, runtime: &dyn Runtime) -> Result<Value> {
        let args = self.args.evaluate(runtime)?;
        let unit = args
            .unit
            .map(|v| v.to_kstr().to_string())
            .unwrap_or_else(|| "$".to_string());
        let n = input
            .as_scalar()
            .and_then(|s| s.to_float().or_else(|| s.to_integer().map(|i| i as f64)))
            .ok_or_else(|| Error::with_msg("number_to_currency: input is not a number"))?;
        let formatted = format!("{n:.2}");
        let (int_part, frac) = formatted.split_once('.').unwrap_or((formatted.as_str(), "00"));
        let grouped = group_digits(int_part, ",");
        Ok(Value::scalar(format!("{unit}{grouped}.{frac}")))
    }
}

// ---------------------------------------------------------------------------
// Array helpers
// ---------------------------------------------------------------------------

#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(name = "sum", description = "Sums an array of numbers (or a property).", parsed(SumFilter))]
pub struct Sum;

#[derive(Debug, Default, Display_filter)]
#[name = "sum"]
struct SumFilter;

impl Filter for SumFilter {
    fn evaluate(&self, input: &dyn ValueView, _runtime: &dyn Runtime) -> Result<Value> {
        let mut total = 0f64;
        let mut int_only = true;
        if let Some(items) = input.as_array() {
            for item in items.values() {
                let Some(n) = item.as_scalar().and_then(|s| s.to_float().or_else(|| s.to_integer().map(|i| i as f64))) else {
                    continue;
                };
                if n.fract() != 0.0 {
                    int_only = false;
                }
                total += n;
            }
        }
        if int_only { Ok(Value::scalar(total as i64)) } else { Ok(Value::scalar(total)) }
    }
}

#[derive(Debug, FilterParameters)]
struct WhereExpArgs {
    #[parameter(description = "Loop variable name.", arg_type = "str")]
    var: liquid_core::Expression,
    #[parameter(description = "Condition, e.g. `item.v > 1`.", arg_type = "str")]
    cond: liquid_core::Expression,
}

#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(
    name = "where_exp",
    description = "Filters an array by a condition on each item.",
    parameters(WhereExpArgs),
    parsed(WhereExpFilter)
)]
pub struct WhereExp;

#[derive(Debug, FromFilterParameters, Display_filter)]
#[name = "where_exp"]
struct WhereExpFilter {
    #[parameters]
    args: WhereExpArgs,
}

fn json_path<'a>(v: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut cur = v;
    for seg in path.split('.') {
        let mut name = seg;
        let mut idx = None;
        if let Some(br) = seg.find('[') {
            name = &seg[..br];
            let rest = seg[br + 1..].trim_end_matches(']');
            idx = rest.parse::<usize>().ok();
        }
        if !name.is_empty() {
            cur = cur.get(name)?;
        }
        if let Some(i) = idx {
            cur = cur.get(i)?;
        }
    }
    Some(cur)
}

fn compare(jv: &serde_json::Value, op: &str, lit: &str) -> bool {
    let lit = lit.trim();
    let lit_str = lit.trim_matches(|c| c == '"' || c == '\'');
    let lit_num = lit_str.parse::<f64>().ok();
    let ord: Option<std::cmp::Ordering> = match (jv, lit_num) {
        (serde_json::Value::Number(n), Some(l)) => n.as_f64().and_then(|n| n.partial_cmp(&l)),
        (serde_json::Value::Bool(b), _) => Some(b.to_string().cmp(&lit_str.to_string())),
        (serde_json::Value::String(s), _) => Some(s.as_str().cmp(lit_str)),
        (serde_json::Value::Null, _) => Some("".cmp(lit_str)),
        _ => None,
    };
    match (ord, op) {
        (Some(std::cmp::Ordering::Less), "<" | "<=" | "!=")
        | (Some(std::cmp::Ordering::Greater), ">" | ">=" | "!=")
        | (Some(std::cmp::Ordering::Equal), "=" | "==" | ">=" | "<=") => true,
        _ => false,
    }
}

impl Filter for WhereExpFilter {
    fn evaluate(&self, input: &dyn ValueView, runtime: &dyn Runtime) -> Result<Value> {
        let args = self.args.evaluate(runtime)?;
        let cond = args.cond.as_str().trim().to_string();
        let re = regex_lite::Regex::new(
            r"^([A-Za-z_][A-Za-z0-9_]*)\s*\.\s*([A-Za-z0-9_.\[\]]+)\s*(==|!=|>=|<=|=>|=<|>|<)\s*(.+)$",
        )
        .expect("valid regex");
        let caps = re
            .captures(&cond)
            .ok_or_else(|| Error::with_msg(format!("where_exp: unsupported condition `{cond}`")))?;
        let (_, [_, path, op, lit]) = caps.extract();
        let op = if op == "=>" { ">=" } else if op == "=<" { "<=" } else { op };
        let items: Vec<Value> = input.as_array().map(|a| a.values().map(|v| v.to_value()).collect())
            .unwrap_or_default();
        let kept: Vec<Value> = items
            .into_iter()
            .filter(|item| {
                let Ok(j) = serde_json::to_value(item) else { return false };
                json_path(&j, path).is_some_and(|v| compare(v, op, lit))
            })
            .collect();
        Ok(Value::array(kept))
    }
}

// ---------------------------------------------------------------------------
// Random helpers (TRMNL cloud)
// ---------------------------------------------------------------------------

fn rand_u64() -> u64 {
    RandomState::new().build_hasher().finish()
}

#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(name = "sample", description = "Picks a random element of an array.", parsed(SampleFilter))]
pub struct Sample;

#[derive(Debug, Default, Display_filter)]
#[name = "sample"]
struct SampleFilter;

impl Filter for SampleFilter {
    fn evaluate(&self, input: &dyn ValueView, _runtime: &dyn Runtime) -> Result<Value> {
        let items: Vec<Value> = input
            .as_array()
            .map(|a| a.values().map(|v| v.to_value()).collect())
            .unwrap_or_default();
        if items.is_empty() {
            return Err(Error::with_msg("sample: empty array"));
        }
        let idx = (rand_u64() as usize) % items.len();
        Ok(items.into_iter().nth(idx).unwrap())
    }
}

#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(
    name = "append_random",
    description = "Appends a random hex token (cache-buster).",
    parsed(AppendRandomFilter)
)]
pub struct AppendRandom;

#[derive(Debug, Default, Display_filter)]
#[name = "append_random"]
struct AppendRandomFilter;

impl Filter for AppendRandomFilter {
    fn evaluate(&self, input: &dyn ValueView, _runtime: &dyn Runtime) -> Result<Value> {
        Ok(Value::scalar(format!("{}{:08x}", input.to_kstr(), (rand_u64() & 0xffff_ffff) as u32)))
    }
}

// ---------------------------------------------------------------------------
// base64 + QR
// ---------------------------------------------------------------------------

#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(name = "base64_encode", description = "Base64-encodes the input.", parsed(B64EncodeFilter))]
pub struct Base64Encode;

#[derive(Debug, Default, Display_filter)]
#[name = "base64_encode"]
struct B64EncodeFilter;

impl Filter for B64EncodeFilter {
    fn evaluate(&self, input: &dyn ValueView, _runtime: &dyn Runtime) -> Result<Value> {
        use base64::Engine;
        Ok(Value::scalar(
            base64::engine::general_purpose::STANDARD.encode(input.to_kstr().as_bytes()),
        ))
    }
}

#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(name = "base64_decode", description = "Base64-decodes the input.", parsed(B64DecodeFilter))]
pub struct Base64Decode;

#[derive(Debug, Default, Display_filter)]
#[name = "base64_decode"]
struct B64DecodeFilter;

impl Filter for B64DecodeFilter {
    fn evaluate(&self, input: &dyn ValueView, _runtime: &dyn Runtime) -> Result<Value> {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(input.to_kstr().as_bytes())
            .map_err(|e| Error::with_msg(format!("base64_decode: {e}")))?;
        Ok(Value::scalar(String::from_utf8_lossy(&bytes).into_owned()))
    }
}

#[derive(Debug, FilterParameters)]
struct QrArgs {
    #[parameter(description = "Module scale.", arg_type = "float")]
    scale: Option<liquid_core::Expression>,
    #[parameter(description = "Error correction level (l/m/q/h).", arg_type = "str")]
    level: Option<liquid_core::Expression>,
}

#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(
    name = "qr_code",
    description = "Renders the input as an inline SVG QR code.",
    parameters(QrArgs),
    parsed(QrFilter)
)]
pub struct QrCode;

#[derive(Debug, FromFilterParameters, Display_filter)]
#[name = "qr_code"]
struct QrFilter {
    #[parameters]
    args: QrArgs,
}

impl Filter for QrFilter {
    fn evaluate(&self, input: &dyn ValueView, runtime: &dyn Runtime) -> Result<Value> {
        let args = self.args.evaluate(runtime)?;
        let scale = args.scale.unwrap_or(4.0) as u32;
        let level = args
            .level
            .map(|v| v.to_kstr().to_ascii_lowercase())
            .unwrap_or_else(|| "m".into());
        let ec = match level.chars().next().unwrap_or('m') {
            'l' => qrcode::EcLevel::L,
            'q' => qrcode::EcLevel::Q,
            'h' => qrcode::EcLevel::H,
            _ => qrcode::EcLevel::M,
        };
        let code = qrcode::QrCode::with_error_correction_level(input.to_kstr().as_bytes(), ec)
            .map_err(|e| Error::with_msg(format!("qr_code: {e}")))?;
        let svg = code
            .render::<qrcode::render::svg::Color>()
            .min_dimensions(scale.max(1), scale.max(1))
            .build();
        Ok(Value::scalar(svg))
    }
}

// ---------------------------------------------------------------------------
// Aliases
// ---------------------------------------------------------------------------

#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(name = "trim", description = "Strips surrounding whitespace.", parsed(TrimFilter))]
pub struct Trim;

#[derive(Debug, Default, Display_filter)]
#[name = "trim"]
struct TrimFilter;

impl Filter for TrimFilter {
    fn evaluate(&self, input: &dyn ValueView, _runtime: &dyn Runtime) -> Result<Value> {
        Ok(Value::scalar(input.to_kstr().trim().to_string()))
    }
}

#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(name = "lowercase", description = "Downcases the input.", parsed(LowercaseFilter))]
pub struct Lowercase;

#[derive(Debug, Default, Display_filter)]
#[name = "lowercase"]
struct LowercaseFilter;

impl Filter for LowercaseFilter {
    fn evaluate(&self, input: &dyn ValueView, _runtime: &dyn Runtime) -> Result<Value> {
        Ok(Value::scalar(input.to_kstr().to_lowercase()))
    }
}

#[derive(Debug, FilterParameters)]
struct PluralizeArgs {
    #[parameter(description = "Singular form.", arg_type = "str")]
    singular: liquid_core::Expression,
    #[parameter(description = "Plural form.", arg_type = "str")]
    plural: Option<liquid_core::Expression>,
}

#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(
    name = "pluralize",
    description = "Pluralizes a word based on the input count.",
    parameters(PluralizeArgs),
    parsed(PluralizeFilter)
)]
pub struct Pluralize;

#[derive(Debug, FromFilterParameters, Display_filter)]
#[name = "pluralize"]
struct PluralizeFilter {
    #[parameters]
    args: PluralizeArgs,
}

impl Filter for PluralizeFilter {
    fn evaluate(&self, input: &dyn ValueView, runtime: &dyn Runtime) -> Result<Value> {
        let args = self.args.evaluate(runtime)?;
        let n = input
            .as_scalar()
            .and_then(|s| s.to_float().or_else(|| s.to_integer().map(|i| i as f64)))
            .unwrap_or(0.0);
        let singular = args.singular.as_str().to_string();
        let plural = args
            .plural
            .map(|v| v.to_kstr().to_string())
            .unwrap_or_else(|| format!("{singular}s"));
        Ok(Value::scalar(if n == 1.0 { singular } else { plural }))
    }
}

// ---------------------------------------------------------------------------
// {% echo expr %} tag (Shopify; used inside {% liquid %})
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, Default)]
pub struct EchoTag;

impl TagReflection for EchoTag {
    fn tag(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Outputs an expression."
    }
}

impl ParseTag for EchoTag {
    fn parse(
        &self,
        mut arguments: TagTokenIter<'_>,
        options: &Language,
    ) -> Result<Box<dyn Renderable>> {
        let src = arguments
            .expect_next("Expression expected.")?
            .expect_filter_chain(options)
            .into_result()?;
        arguments.expect_nothing()?;
        Ok(Box::new(EchoChain(src)))
    }

    fn reflection(&self) -> &dyn TagReflection {
        self
    }
}

#[derive(Debug)]
struct EchoChain(liquid_core::parser::FilterChain);

impl Renderable for EchoChain {
    fn render_to(&self, writer: &mut dyn Write, runtime: &dyn Runtime) -> Result<()> {
        let value = self
            .0
            .evaluate(runtime)
            .trace_with(|| "{% echo %}".to_owned().into())?;
        write!(writer, "{}", value.to_kstr())
            .map_err(|e| Error::with_msg(format!("echo: {e}")))
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the whole compat pack into a parse language (overriding the
/// stdlib `date`).
pub fn register_into(lang: &mut liquid_core::parser::Language) {
    for f in [
        Box::new(Date) as Box<dyn liquid_core::ParseFilter>,
        LDate.into(),
        DateWithLocalTime.into(),
        ToDate.into(),
        Jsonify.into(),
        JsonFilterParse.into(),
        ParseJson.into(),
        NumberWithDelimiter.into(),
        NumberToCurrency.into(),
        Sum.into(),
        WhereExp.into(),
        Sample.into(),
        AppendRandom.into(),
        Base64Encode.into(),
        Base64Decode.into(),
        QrCode.into(),
        Trim.into(),
        Lowercase.into(),
        Pluralize.into(),
    ] {
        lang.filters.register(f.reflection().name().to_owned(), f);
    }
}
