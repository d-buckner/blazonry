//! The screen tree: what a Liquid template compiles to, and what the
//! renderer consumes. ~15 primitives cover every TRMNL screen family.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Row,
    Column,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Style {
    /// 0 = black, 1 = dark grey, 2 = light grey, 3 = white (e-ink 4-level).
    #[serde(default = "default_fg")]
    pub fg: u8,
    #[serde(default = "default_bg")]
    pub bg: u8,
    #[serde(default = "default_font_px")]
    pub font_px: f32,
    #[serde(default)]
    pub bold: bool,
}

fn default_fg() -> u8 { 0 }
fn default_bg() -> u8 { 3 }
fn default_font_px() -> f32 { 24.0 }

impl Default for Style {
    fn default() -> Self {
        Style { fg: default_fg(), bg: default_bg(), font_px: default_font_px(), bold: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    #[serde(default)]
    pub style: Style,
    /// Padding in px: [top, right, bottom, left].
    #[serde(default, with = "array_vec")]
    pub padding: [f32; 4],
    #[serde(default)]
    pub gap: f32,
    #[serde(default)]
    pub align: Align,
    /// Flex grow factor (GPUI/taffy vocabulary).
    #[serde(default)]
    pub grow: f32,
    /// Fixed width/height in px (None = fit content).
    #[serde(default)]
    pub width: Option<f32>,
    #[serde(default)]
    pub height: Option<f32>,
    /// Draw a 1px border in fg color.
    #[serde(default)]
    pub border: bool,
    #[serde(flatten)]
    pub kind: NodeKind,
}

mod array_vec {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub(super) fn serialize<S: Serializer>(v: &[f32; 4], s: S) -> Result<S::Ok, S::Error> {
        v.as_slice().serialize(s)
    }
    pub(super) fn deserialize<'de, D>(d: D) -> Result<[f32; 4], D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = Vec::<f32>::deserialize(d)?;
        match v.len() {
            1 => Ok([v[0]; 4]),
            2 => Ok([v[0], v[1], v[0], v[1]]),
            4 => Ok([v[0], v[1], v[2], v[3]]),
            n => Err(serde::de::Error::invalid_length(n, &"1, 2 or 4 paddings")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NodeKind {
    /// Text line. Supports `{{var}}` already resolved by Liquid.
    Text { text: String },
    /// Flex container.
    Flex { direction: Direction, children: Vec<Node> },
    /// Horizontal rule.
    Divider,
    /// key/value row — the workhorse of TRMNL screens.
    KeyedValue { key: String, value: String },
    /// Simple vertical bar chart.
    Bars { values: Vec<f32>, labels: Vec<String> },
    /// Progress bar 0.0..=1.0
    Progress { value: f32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Screen {
    pub name: String,
    #[serde(default)]
    pub parts: Vec<Node>,
}
