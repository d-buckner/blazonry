// SPDX-License-Identifier: AGPL-3.0-only
//! Software renderer: Screen tree -> greyscale pixmap.
//!
//! Geometry is taffy 0.15 (CSS flexbox): the screen vocabulary maps onto taffy
//! styles, leaf intrinsic sizes come from fontdue measurements, and we paint
//! the resulting boxes ourselves with tiny-skia. Boxes are taffy's; pixels are
//! ours. (This replaced a hand-rolled measure/paint pass — see issue #4 and the
//! README design note for why.)
//!
//! Text: fontdue per-char raster + manual advance layout (enough for the
//! vocabulary; shaping can graduate to cosmic-text later).

use crate::dither::floyd_steinberg_4;
use crate::screen::{Align, Direction, Node, NodeKind, Screen};
use crate::{HEIGHT, WIDTH};
use std::collections::HashMap;
use std::sync::Mutex;
use taffy::prelude::*;
use taffy::{compute_leaf_layout, LayoutInput};
use taffy::geometry::Rect as TaffyRect;
use tiny_skia::{Color, ColorU8, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

const GREYS: [f32; 4] = [0.0, 0.333, 0.667, 1.0];

/// Gap between key and value in a `keyed_value` row.
const KEYED_GAP: f32 = 24.0;

/// Fonts are embedded so the binary stays self-contained (no fontconfig, no
/// system package). Order of resolution: `BLAZONRY_FONT[_BOLD]` override →
/// embedded bytes → system DejaVu paths (for builds that strip assets).
const EMBEDDED: [(&str, &[u8]); 2] = [
    ("regular", include_bytes!("../assets/fonts/DejaVuSans.ttf")),
    ("bold", include_bytes!("../assets/fonts/DejaVuSans-Bold.ttf")),
];

const SYSTEM: [(&str, &[&str]); 2] = [
    (
        "regular",
        &[
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/local/share/fonts/DejaVuSans.ttf",
        ],
    ),
    (
        "bold",
        &[
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
            "/usr/local/share/fonts/DejaVuSans-Bold.ttf",
        ],
    ),
];

fn load_font(idx: usize, env_var: &str) -> anyhow::Result<fontdue::Font> {
    let opts = fontdue::FontSettings::default();
    let parse = |bytes: &[u8], src: String| -> (Option<fontdue::Font>, String) {
        match fontdue::Font::from_bytes(bytes, opts) {
            Ok(f) => (Some(f), src),
            Err(e) => (None, format!("{src}: {e}")),
        }
    };
    let mut errors = Vec::new();

    // 1. explicit override
    if let Ok(path) = std::env::var(env_var) {
        match std::fs::read(&path) {
            Ok(b) => {
                let (f, src) = parse(&b, format!("{env_var}={path}"));
                if let Some(f) = f {
                    return Ok(f);
                }
                errors.push(src);
            }
            Err(e) => errors.push(format!("{env_var}={path}: {e}")),
        }
    }

    // 2. embedded bytes
    let (name, embedded) = EMBEDDED[idx];
    let (f, src) = parse(embedded, format!("embedded {name}"));
    if let Some(f) = f {
        return Ok(f);
    }
    errors.push(src);

    // 3. system paths
    for p in SYSTEM[idx].1 {
        match std::fs::read(p) {
            Ok(b) => {
                let (f, src) = parse(&b, p.to_string());
                if let Some(f) = f {
                    tracing::warn!("using system font {p}");
                    return Ok(f);
                }
                errors.push(src);
            }
            Err(e) => errors.push(format!("{p}: {e}")),
        }
    }
    Err(anyhow::anyhow!("no usable font: {}", errors.join("; ")))
}

/// True for leaves that fill their container's content width (the vocabulary
/// treats divider/progress/bars as full-width rules, not content-sized).
fn is_stretchy(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Divider | NodeKind::Progress { .. } | NodeKind::Bars { .. }
    )
}

/// Cached text advance: (width, height) per (text, px, bold) key.
type TextAdvance = (f32, f32);

pub struct Renderer {
    font: fontdue::Font,
    bold: fontdue::Font,
    /// taffy may measure the same leaf repeatedly (min/max-content passes);
    /// fontdue rasterizes per call, so cache advances per (text, px, bold).
    text_cache: Mutex<HashMap<(String, u32, bool), TextAdvance>>,
}

impl Renderer {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Renderer {
            font: load_font(0, "BLAZONRY_FONT")?,
            bold: load_font(1, "BLAZONRY_FONT_BOLD")?,
            text_cache: Mutex::new(HashMap::new()),
        })
    }

    /// Render a screen to a 2-bit index buffer (row-major, top-down).
    pub fn render(&self, screen: &Screen) -> Vec<u8> {
        let mut pm = Pixmap::new(WIDTH as u32, HEIGHT as u32).unwrap();
        pm.fill(Color::from_rgba8(255, 255, 255, 255));

        // --- taffy tree -------------------------------------------------------
        // Root is a column; each top-level part sits inside a full-width
        // wrapper that carries the part's `align`. A content-sized leaf has no
        // slack of its own for align to act on — the wrapper gives it some,
        // which is where flex puts alignment (the old pass faked it inside
        // each leaf's own box).
        let mut taffy = TaffyTree::<usize>::new();
        taffy.disable_rounding(); // the old pass never snapped; dither shows seams
        let root = taffy
            .new_with_children(
                Style {
                    display: Display::Flex,
                    flex_direction: FlexDirection::Column,
                    size: Size { width: length::<f32, Dimension>(WIDTH as f32), height: auto() },
                    ..Default::default()
                },
                &[],
            )
            .expect("root node");

        // Each leaf carries its vocabulary node's index (into the flattened
        // tree) as its taffy NodeContext, so the measure callback can find it.
        let mut registry: Vec<*const Node> = Vec::new();
        let mut wrappers: Vec<NodeId> = Vec::with_capacity(screen.parts.len());
        {
            fn build(taffy: &mut TaffyTree<usize>, node: &Node, registry: &mut Vec<*const Node>, top_level: bool) -> NodeId {
                let style = node_style(node, top_level);
                match &node.kind {
                    NodeKind::Flex { children, .. } => {
                        let kids: Vec<NodeId> =
                            children.iter().map(|c| build(taffy, c, registry, false)).collect();
                        taffy.new_with_children(style, &kids).expect("flex node")
                    }
                    _ => {
                        let idx = registry.len();
                        registry.push(node as *const Node);
                        taffy.new_leaf_with_context(style, idx)
                            .expect("leaf node")
                    }
                }
            }
            for part in &screen.parts {
                let child = build(&mut taffy, part, &mut registry, true);
                let wrapper = taffy
                    .new_with_children(wrapper_style(part), &[child])
                    .expect("wrapper node");
                taffy.add_child(root, wrapper).expect("add part");
                wrappers.push(wrapper);
            }
        }

        taffy
            .compute_layout_with_measure(
                root,
                Size {
                    width: AvailableSpace::Definite(WIDTH as f32),
                    height: AvailableSpace::MaxContent,
                },
                |inputs: LayoutInput, _id, ctx, style| {
                    // SAFETY: every registry pointer points into `screen`,
                    // which is borrowed for the whole call.
                    let node: &Node = ctx
                        .and_then(|c| registry.get(*c).map(|p| unsafe { &**p }))
                        .unwrap_or(&EMPTY_NODE);
                    compute_leaf_layout(inputs, style, |_, v| v, |_known, _avail| {
                        self.leaf_measure(node)
                    })
                },
            )
            .expect("taffy layout");

        // --- paint ------------------------------------------------------------
        for (wrapper, part) in wrappers.iter().zip(&screen.parts) {
            let child = taffy.child_at_index(*wrapper, 0).expect("child");
            let wl = taffy.unrounded_layout(*wrapper);
            self.paint_node(&taffy, child, part, &mut pm, wl.location.x, wl.location.y);
        }

        // greyscale from premultiplied RGBA (opaque paint, so r == grey)
        let grey: Vec<u8> = pm.pixels().iter().map(|p| p.red()).collect();
        floyd_steinberg_4(&grey, WIDTH, HEIGHT)
    }

    /// Intrinsic content size of a leaf (no padding — taffy adds it).
    /// `avail_w` is the width the container offers; stretchy leaves report 0
    /// width and rely on stretch, text leaves report their natural advance.
    fn leaf_measure(&self, node: &Node) -> Size<f32> {
        match &node.kind {
            NodeKind::Text { text } => {
                let (w, h) = self.measure_text(text, node);
                Size { width: w, height: h }
            }
            NodeKind::KeyedValue { key, value } => {
                let (kw, kh) = self.measure_text(key, node);
                let (vw, vh) = self.measure_text(value, node);
                Size { width: kw + vw + KEYED_GAP, height: kh.max(vh) }
            }
            NodeKind::Divider => Size { width: 0.0, height: 2.0 },
            NodeKind::Progress { .. } => Size { width: 0.0, height: 28.0 },
            NodeKind::Bars { .. } => Size { width: 0.0, height: 140.0 },
            NodeKind::Flex { .. } => Size { width: 0.0, height: 0.0 },
        }
    }

    fn font_for(&self, node: &Node) -> &fontdue::Font {
        if node.style.bold { &self.bold } else { &self.font }
    }

    fn measure_text(&self, text: &str, node: &Node) -> (f32, f32) {
        let key = (text.to_string(), node.style.font_px.to_bits(), node.style.bold);
        if let Some(&m) = self.text_cache.lock().expect("text cache").get(&key) {
            return m;
        }
        let font = self.font_for(node);
        let px = node.style.font_px;
        let mut w = 0.0f32;
        for ch in text.chars() {
            let (m, _) = font.rasterize(ch, px);
            w += m.advance_width;
        }
        let m = (w, px * 1.35);
        self.text_cache.lock().expect("text cache").insert(key, m);
        m
    }

    /// Horizontal offset that a leaf's own `align` produces inside its box.
    /// Top-level alignment is handled by the wrapper; this only matters for
    /// leaves whose box is wider than their content (explicit `width`).
    /// Clamped, so content wider than its box stays at the origin.
    fn align_dx(node: &Node, box_w: f32, content_w: f32) -> f32 {
        let slack = (box_w - content_w).max(0.0);
        match node.align {
            Align::Start => 0.0,
            Align::Center => slack / 2.0,
            Align::End => slack,
        }
    }

    /// Paint a taffy-laid-out node. `ox`/`oy` are the parent's absolute
    /// top-left (taffy locations are parent-relative, so we accumulate).
    fn paint_node(
        &self,
        taffy: &TaffyTree<usize>,
        id: NodeId,
        node: &Node,
        pm: &mut Pixmap,
        ox: f32,
        oy: f32,
    ) {
        let layout = taffy.unrounded_layout(id);
        let x = ox + layout.location.x;
        let y = oy + layout.location.y;
        let w = layout.size.width;
        let h = layout.size.height;

        let pad = node.padding;
        let fg = grey_color(node.style.fg);
        // Node background: bg defaults to 3 (white = page ground), so only
        // non-white levels pay for a fill. This is what makes inverted bands
        // and dark cards possible on a 4-level panel.
        if node.style.bg % 4 != 3 {
            fill_rect(pm, x, y, w, h, grey_color(node.style.bg));
        }
        if node.border {
            let stroke = Stroke { width: 1.0, ..Default::default() };
            let mut p = Paint::default();
            p.set_color(fg);
            let mut pb = PathBuilder::new();
            if let Some(r) = Rect::from_xywh(x + 0.5, y + 0.5, (w - 1.0).max(1.0), (h - 1.0).max(1.0)) {
                pb.push_rect(r);
                if let Some(path) = pb.finish() {
                    pm.stroke_path(&path, &p, &stroke, Transform::default(), None);
                }
            }
        }
        let (ix, iy, iw) = (x + pad[3], y + pad[0], (w - pad[1] - pad[3]).max(0.0));
        match &node.kind {
            NodeKind::Text { text } => {
                let (tw, _) = self.measure_text(text, node);
                let dx = Self::align_dx(node, iw, tw);
                self.draw_text(pm, text, (ix + dx).max(0.0), iy, node);
            }
            NodeKind::KeyedValue { key, value } => {
                let (kw, _) = self.measure_text(key, node);
                let (vw, _) = self.measure_text(value, node);
                let dx = Self::align_dx(node, iw, kw + KEYED_GAP + vw);
                self.draw_text(pm, key, (ix + dx).max(0.0), iy, node);
                self.draw_text(pm, value, (ix + dx + kw + KEYED_GAP).max(0.0), iy, node);
            }
            NodeKind::Divider => fill_rect(pm, ix, iy + 1.0, iw, 1.0, fg),
            NodeKind::Progress { value } => {
                let mut p = Paint::default();
                p.set_color(fg);
                if let Some(r) =
                    Rect::from_xywh(ix, iy + 6.0, (iw * value.clamp(0.0, 1.0)).max(0.5), 16.0)
                {
                    pm.fill_rect(r, &p, Transform::default(), None);
                }
                let s = Stroke { width: 1.0, ..Default::default() };
                let mut pb = PathBuilder::new();
                if let Some(frame) = Rect::from_xywh(ix, iy + 6.0, iw, 16.0) {
                    pb.push_rect(frame);
                    if let Some(path) = pb.finish() {
                        pm.stroke_path(&path, &p, &s, Transform::default(), None);
                    }
                }
            }
            NodeKind::Bars { values, labels } => {
                if values.is_empty() {
                    return;
                }
                let max = values.iter().cloned().fold(f32::MIN, f32::max).max(1.0);
                let bw = iw / values.len() as f32;
                let chart_h = 110.0;
                let mut p = Paint::default();
                p.set_color(fg);
                for (i, v) in values.iter().enumerate() {
                    let bh = (chart_h * (v / max)).max(1.0);
                    if let Some(r) = Rect::from_xywh(
                        ix + i as f32 * bw + 2.0,
                        iy + chart_h - bh,
                        (bw - 4.0).max(1.0),
                        bh,
                    ) {
                        pm.fill_rect(r, &p, Transform::default(), None);
                    }
                }
                for (i, l) in labels.iter().enumerate() {
                    self.draw_text(pm, l, ix + i as f32 * bw + 2.0, iy + chart_h + 6.0, node);
                }
            }
            NodeKind::Flex { children, .. } => {
                let kids = taffy.children(id).expect("children");
                for (child, cnode) in kids.iter().zip(children.iter()) {
                    self.paint_node(taffy, *child, cnode, pm, x, y);
                }
            }
        }
    }

    fn draw_text(&self, pm: &mut Pixmap, text: &str, x: f32, y: f32, node: &Node) {
        let font = self.font_for(node);
        let px = node.style.font_px;
        let grey = (GREYS[node.style.fg as usize % 4] * 255.0) as u8;
        let baseline = y + px; // simple baseline: top + font size
        let mut pen_x = x;
        for ch in text.chars() {
            let (m, coverage) = font.rasterize(ch, px);
            for gy in 0..m.height {
                for gx in 0..m.width {
                    let cov = coverage[gy * m.width + gx] as u32;
                    if cov == 0 {
                        continue;
                    }
                    let px_x = (pen_x + m.xmin as f32 + gx as f32) as i32;
                    let px_y = (baseline - m.ymin as f32 - m.height as f32 + gy as f32) as i32;
                    if px_x < 0 || px_y < 0 {
                        continue;
                    }
                    if let Some(p) = pm.pixels_mut().get_mut(px_y as usize * WIDTH + px_x as usize) {
                        let bg = p.red() as u32;
                        let v = (bg * (255 - cov) + grey as u32 * cov) / 255;
                        *p = ColorU8::from_rgba(v as u8, v as u8, v as u8, 255).premultiply();
                    }
                }
            }
            pen_x += m.advance_width;
        }
    }
}

/// Fallback for measure calls on nodes we did not register (shouldn't happen;
/// keeps the callback infallible).
static EMPTY_NODE: std::sync::LazyLock<Node> = std::sync::LazyLock::new(|| Node {
    style: Default::default(),
    padding: [0.0; 4],
    gap: 0.0,
    align: Align::Start,
    grow: 0.0,
    width: None,
    height: None,
    border: false,
    kind: NodeKind::Text { text: String::new() },
});

/// Taffy style for a vocabulary node (flex containers and leaves alike).
/// `width`/`height` are border-box, matching the vocabulary's contract.
fn node_style(node: &Node, top_level: bool) -> Style {
    let mut s = Style {
        padding: TaffyRect {
            top: length(node.padding[0]),
            right: length(node.padding[1]),
            bottom: length(node.padding[2]),
            left: length(node.padding[3]),
        },
        box_sizing: BoxSizing::BorderBox,
        flex_grow: node.grow.max(0.0),
        ..Default::default()
    };
    if node.grow > 0.0 {
        // The old pass handed grow children exactly the leftover space even
        // when their content was wider; CSS min-width:auto would refuse.
        s.min_size.width = length::<f32, LengthPercentageAuto>(0.0);
    }
    match &node.kind {
        NodeKind::Flex { direction, .. } => {
            s.display = Display::Flex;
            s.flex_direction = match direction {
                Direction::Row => FlexDirection::Row,
                Direction::Column => FlexDirection::Column,
            };
            s.gap = Size::<LengthPercentage> { width: length(node.gap), height: length(node.gap) };
            // The old pass gave every top-level part a full-width box; a
            // nested row only fills its parent when it has growing children.
            let wants_full_width = node.width.is_none()
                && (top_level || children_have_grow(node));
            if wants_full_width {
                s.align_self = Some(AlignSelf::STRETCH);
            }
            // The vocabulary's `align` means "shift content along the main
            // axis in a row, across it in a column" — same mapping taffy uses.
            match direction {
                Direction::Row => s.justify_content = justify(node.align),
                Direction::Column => s.align_items = items(node.align),
            };
        }
        _ => {
            // Stretchy leaves fill the container's content width — but an
            // explicit `width` wins (stretch is ignored when width is set).
            if is_stretchy(&node.kind) && node.width.is_none() {
                s.align_self = Some(AlignSelf::STRETCH);
            }
        }
    }
    s.size.width = node.width.map(length::<f32, Dimension>).unwrap_or(auto());
    s.size.height = node.height.map(length::<f32, Dimension>).unwrap_or(auto());
    s
}

fn children_have_grow(node: &Node) -> bool {
    match &node.kind {
        NodeKind::Flex { children, .. } => children.iter().any(|c| c.grow > 0.0),
        _ => false,
    }
}

/// Full-width wrapper around each top-level part: gives `align` somewhere to
/// act (a content-sized leaf has no slack of its own). Stretchy parts fill
/// the wrapper regardless of align — divider/progress/bars are full-width
/// rules in this vocabulary.
fn wrapper_style(part: &Node) -> Style {
    Style {
        display: Display::Flex,
        flex_direction: FlexDirection::Column,
        size: Size { width: length::<f32, Dimension>(WIDTH as f32), height: auto() },
        align_items: items(part.align),
        ..Default::default()
    }
}

fn justify(a: Align) -> JustifyContent {
    match a {
        Align::Start => JustifyContent::START,
        Align::Center => JustifyContent::CENTER,
        Align::End => JustifyContent::END,
    }
}

fn items(a: Align) -> AlignItems {
    match a {
        Align::Start => AlignItems::START,
        Align::Center => AlignItems::CENTER,
        Align::End => AlignItems::END,
    }
}

fn grey_color(level: u8) -> Color {
    let g = GREYS[level as usize % 4];
    Color::from_rgba8((g * 255.0) as u8, (g * 255.0) as u8, (g * 255.0) as u8, 255)
}

fn fill_rect(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, color: Color) {
    let mut p = Paint::default();
    p.set_color(color);
    if let Some(r) = Rect::from_xywh(x, y, w, h) {
        pm.fill_rect(r, &p, Transform::default(), None);
    }
}
