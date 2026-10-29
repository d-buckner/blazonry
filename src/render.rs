//! Software renderer: Screen tree -> greyscale pixmap.
//!
//! Two passes like GPUI: measure (returns size) then paint (given bounds).
//! Flex layout is hand-rolled (single-axis distribution + grow) — enough for
//! the screen vocabulary without pulling a full taffy pass into the hot path.
//! Text: fontdue per-char raster + manual advance layout (enough for the
//! vocabulary; shaping can graduate to cosmic-text later).

use crate::dither::floyd_steinberg_4;
use crate::screen::{Align, Direction, Node, NodeKind, Screen};
use crate::{HEIGHT, WIDTH};
use tiny_skia::{Color, ColorU8, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

const GREYS: [f32; 4] = [0.0, 0.333, 0.667, 1.0];

pub struct Renderer {
    font: fontdue::Font,
    bold: fontdue::Font,
}

impl Renderer {
    pub fn new() -> anyhow::Result<Self> {
        // Bundled in a real build; system fonts are fine for the spike.
        let regular = std::fs::read("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf")
            .map_err(|e| anyhow::anyhow!("font: {e}"))?;
        let bold = std::fs::read("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf")
            .map_err(|e| anyhow::anyhow!("font: {e}"))?;
        let opts = fontdue::FontSettings::default();
        Ok(Renderer {
            font: fontdue::Font::from_bytes(&regular[..], opts).map_err(|e| anyhow::anyhow!("font: {e}"))?,
            bold: fontdue::Font::from_bytes(&bold[..], opts).map_err(|e| anyhow::anyhow!("font: {e}"))?,
        })
    }

    /// Render a screen to a 2-bit index buffer (row-major, top-down).
    pub fn render(&self, screen: &Screen) -> Vec<u8> {
        let mut pm = Pixmap::new(WIDTH as u32, HEIGHT as u32).unwrap();
        pm.fill(Color::from_rgba8(255, 255, 255, 255));
        let mut y = 0.0f32;
        for part in &screen.parts {
            let m = self.measure(part, WIDTH as f32);
            self.paint(part, &mut pm, 0.0, y, m.0.max(WIDTH as f32));
            y += m.1;
        }
        // greyscale from premultiplied RGBA (opaque paint, so r == grey)
        let grey: Vec<u8> = pm.pixels().iter().map(|p| p.red()).collect();
        floyd_steinberg_4(&grey, WIDTH, HEIGHT)
    }

    fn font_for(&self, node: &Node) -> &fontdue::Font {
        if node.style.bold { &self.bold } else { &self.font }
    }

    fn measure_text(&self, text: &str, node: &Node) -> (f32, f32) {
        let font = self.font_for(node);
        let px = node.style.font_px;
        let mut w = 0.0f32;
        let mut adv_max = 0.0f32;
        for ch in text.chars() {
            let (m, _) = font.rasterize(ch, px);
            w += m.advance_width;
            adv_max = adv_max.max(m.advance_width);
        }
        _ = adv_max;
        (w, px * 1.35)
    }

    fn measure(&self, node: &Node, max_w: f32) -> (f32, f32) {
        let pad = node.padding;
        let inner_w = (max_w - pad[1] - pad[3]).max(0.0);
        let size = match &node.kind {
            NodeKind::Text { text } => self.measure_text(text, node),
            NodeKind::KeyedValue { key, value } => {
                let (kw, kh) = self.measure_text(key, node);
                let (vw, vh) = self.measure_text(value, node);
                (kw + vw + 24.0, kh.max(vh))
            }
            NodeKind::Divider => (max_w, 2.0),
            NodeKind::Progress { .. } => (max_w, 28.0),
            NodeKind::Bars { .. } => (max_w, 140.0),
            NodeKind::Flex { direction, children } => match direction {
                Direction::Column => {
                    let mut h = 0.0f32;
                    let mut w = 0.0f32;
                    for c in children {
                        let m = self.measure(c, inner_w);
                        h += m.1;
                        w = w.max(m.0);
                    }
                    h += node.gap * children.len().saturating_sub(1) as f32;
                    (w, h)
                }
                Direction::Row => {
                    let grows = children.iter().any(|c| c.grow > 0.0);
                    let mut w = 0.0f32;
                    let mut h = 0.0f32;
                    for c in children {
                        let m = self.measure(c, if c.grow > 0.0 { 1.0 } else { inner_w });
                        w += m.0;
                        h = h.max(m.1);
                    }
                    w += node.gap * children.len().saturating_sub(1) as f32;
                    if grows {
                        w = max_w;
                    }
                    (w, h)
                }
            },
        };
    (size.0 + pad[1] + pad[3], size.1 + pad[0] + pad[2])
    }

    /// Paint node at (x, y) into a box of width `w`.
    fn paint(&self, node: &Node, pm: &mut Pixmap, x: f32, y: f32, w: f32) {
        let pad = node.padding;
        let fg = grey_color(node.style.fg);
        if node.border {
            let mut stroke = Stroke::default();
            stroke.width = 1.0;
            let mut p = Paint::default();
            p.set_color(fg);
            let h = self.measure(node, w).1;
            let mut pb = PathBuilder::new();
            {
                let r = Rect::from_xywh(x + 0.5, y + 0.5, (w - 1.0).max(1.0), (h - 1.0).max(1.0));
                if let Some(r) = r {
                    pb.push_rect(r);
                    if let Some(path) = pb.finish() {
                        pm.stroke_path(&path, &p, &stroke, Transform::default(), None);
                    }
                }
            }
        }
        let (ix, iy, iw) = (x + pad[3], y + pad[0], (w - pad[1] - pad[3]).max(0.0));
        match &node.kind {
            NodeKind::Text { text } => self.draw_text(pm, text, ix, iy, node),
            NodeKind::KeyedValue { key, value } => {
                let (kw, _) = self.measure_text(key, node);
                self.draw_text(pm, key, ix, iy, node);
                self.draw_text(pm, value, ix + kw + 24.0, iy, node);
            }
            NodeKind::Divider => fill_rect(pm, ix, iy + 1.0, iw, 1.0, fg),
            NodeKind::Progress { value } => {
                let mut p = Paint::default();
                p.set_color(fg);
                if let Some(r) = Rect::from_xywh(ix, iy + 6.0, (iw * value.clamp(0.0, 1.0)).max(0.5), 16.0) {
                    pm.fill_rect(r, &p, Transform::default(), None);
                }
                let mut s = Stroke::default();
                s.width = 1.0;
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
                    let h = (chart_h * (v / max)).max(1.0);
                    if let Some(r) = Rect::from_xywh(ix + i as f32 * bw + 2.0, iy + chart_h - h, (bw - 4.0).max(1.0), h) {
                        pm.fill_rect(r, &p, Transform::default(), None);
                    }
                }
                for (i, l) in labels.iter().enumerate() {
                    self.draw_text(pm, l, ix + i as f32 * bw + 2.0, iy + chart_h + 6.0, node);
                }
            }
            NodeKind::Flex { direction, children } => match direction {
                Direction::Column => {
                    let mut cy = iy;
                    for c in children {
                        let m = self.measure(c, iw);
                        let cx = match node.align {
                            Align::Start => ix,
                            Align::Center => ix + (iw - m.0) / 2.0,
                            Align::End => ix + iw - m.0,
                        };
                        self.paint(c, pm, cx, cy, m.0);
                        cy += m.1 + node.gap;
                    }
                }
                Direction::Row => {
                    let fixed: f32 = children
                        .iter()
                        .filter(|c| c.grow <= 0.0)
                        .map(|c| self.measure(c, iw).0)
                        .sum::<f32>()
                        + node.gap * children.len().saturating_sub(1) as f32;
                    let grow_sum: f32 = children.iter().map(|c| c.grow.max(0.0)).sum();
                    let grow_w = (iw - fixed).max(0.0);
                    let mut cx = ix;
                    for c in children {
                        let cw = if c.grow > 0.0 && grow_sum > 0.0 {
                            grow_w * c.grow / grow_sum
                        } else {
                            self.measure(c, iw).0
                        };
                        self.paint(c, pm, cx, iy, cw);
                        cx += cw + node.gap;
                    }
                }
            },
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
