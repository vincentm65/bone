//! Text on GPUI: runs shaped by the window's text system (one line of one
//! size at a time), then wrapped, aligned and cut here, so a text can mix
//! sizes and the view knows where each character is. What is painted is a
//! list of glyphs and the boxes under and over them.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{FontId, FontStyle, FontWeight, GlyphId, SharedString, TextRun, px};

use super::geom::{Rect, vec2};
use super::text::{Family, Job, Row, RunStyle, Shaped, Shaper};
use crate::node::{Rgba, TextAlign};

/// A glyph to paint: where its baseline starts, from the text's top left.
#[derive(Clone, Debug)]
pub struct Glyph {
    pub x: f32,
    pub y: f32,
    pub font: FontId,
    pub id: GlyphId,
    pub size: f32,
    pub emoji: bool,
    pub color: Rgba,
}

/// A box with the text: a span's background (under the glyphs), or an
/// underline or a strike (over them).
#[derive(Clone, Debug)]
pub struct Deco {
    pub rect: Rect,
    pub color: Rgba,
    pub under: bool,
}

/// What a [`Shaped`] keeps for GPUI to paint.
#[derive(Default, Debug)]
pub struct Laid {
    pub glyphs: Vec<Glyph>,
    pub decos: Vec<Deco>,
}

/// The families the window's fonts are known by.
#[derive(Clone, Debug)]
pub struct FontNames {
    pub mono: SharedString,
    pub sans: SharedString,
    pub bold: Option<SharedString>,
    pub italic: Option<SharedString>,
    pub named: HashMap<String, SharedString>,
}

impl Default for FontNames {
    fn default() -> Self {
        FontNames {
            mono: "DejaVu Sans Mono".into(),
            sans: "DejaVu Sans".into(),
            bold: None,
            italic: None,
            named: HashMap::new(),
        }
    }
}

pub struct GpuiShaper {
    pub text: Arc<gpui::WindowTextSystem>,
    pub fonts: FontNames,
}

/// A character laid out on one line.
struct Ch {
    w: f32,
    newline: bool,
    space: bool,
    /// Its glyphs, from its left edge.
    glyphs: Vec<(f32, FontId, GlyphId, bool)>,
    run: usize,
    ascent: f32,
    descent: f32,
}

pub fn hsla(c: Rgba) -> gpui::Hsla {
    gpui::Rgba {
        r: f32::from(c.0) / 255.0,
        g: f32::from(c.1) / 255.0,
        b: f32::from(c.2) / 255.0,
        a: f32::from(c.3) / 255.0,
    }
    .into()
}

impl GpuiShaper {
    fn font(&self, st: &RunStyle) -> gpui::Font {
        let (family, weight, style) = match &st.family {
            Family::Mono => (self.fonts.mono.clone(), FontWeight::NORMAL, FontStyle::Normal),
            Family::Sans => (self.fonts.sans.clone(), FontWeight::NORMAL, FontStyle::Normal),
            Family::SansBold => (
                self.fonts.bold.clone().unwrap_or(self.fonts.sans.clone()),
                FontWeight::BOLD,
                FontStyle::Normal,
            ),
            Family::SansItalic => (
                self.fonts.italic.clone().unwrap_or(self.fonts.sans.clone()),
                FontWeight::NORMAL,
                FontStyle::Italic,
            ),
            Family::Named(n) => (
                self.fonts.named.get(n).cloned().unwrap_or(n.clone().into()),
                FontWeight::NORMAL,
                FontStyle::Normal,
            ),
        };
        gpui::Font {
            weight,
            style: if st.italic { FontStyle::Italic } else { style },
            ..gpui::font(family)
        }
    }

    /// One line of one run, as characters.
    fn line(&self, text: &str, st: &RunStyle, run: usize, out: &mut Vec<Ch>) {
        if text.is_empty() {
            return;
        }
        let tr = TextRun {
            len: text.len(),
            font: self.font(st),
            color: hsla(st.color),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let layout = self.text.layout_line(text, px(st.size), &[tr], None);
        // The run's own font's metrics, not a fallback's: a row is as tall
        // as `row_height` says, whatever glyphs it borrows.
        let (_, ascent, descent) = self.metrics(st);
        // Where each char starts: the first glyph at its byte, else where
        // the line says.
        let mut at: HashMap<usize, f32> = HashMap::new();
        let mut glyphs: Vec<(usize, f32, FontId, GlyphId, bool)> = Vec::new();
        for r in &layout.runs {
            for g in &r.glyphs {
                at.entry(g.index).or_insert(f32::from(g.position.x));
                glyphs.push((g.index, f32::from(g.position.x), r.font_id, g.id, g.is_emoji));
            }
        }
        let starts: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        let xs: Vec<f32> = starts
            .iter()
            .map(|b| {
                at.get(b)
                    .copied()
                    .unwrap_or_else(|| f32::from(layout.x_for_index(*b)))
            })
            .collect();
        let width = f32::from(layout.width);
        let first = out.len();
        for (k, (b, c)) in text.char_indices().enumerate() {
            let next = xs.get(k + 1).copied().unwrap_or(width);
            let _ = b;
            out.push(Ch {
                w: (next - xs[k]).max(0.0) + st.letter_spacing,
                newline: false,
                space: c.is_whitespace(),
                glyphs: Vec::new(),
                run,
                ascent,
                descent,
            });
        }
        // Each glyph to the char its byte is in.
        for (byte, x, font, id, emoji) in glyphs {
            let k = match starts.binary_search(&byte) {
                Ok(k) => k,
                Err(k) => k.saturating_sub(1),
            };
            out[first + k].glyphs.push((x - xs[k], font, id, emoji));
        }
    }

    /// The height of a line of `st`, and its ascent and descent.
    fn metrics(&self, st: &RunStyle) -> (f32, f32, f32) {
        // From a shaped line, as the glyphs are placed: the font's own
        // figures can disagree with them.
        let tr = TextRun {
            len: 2,
            font: self.font(st),
            color: hsla(st.color),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let layout = self.text.layout_line("Mg", px(st.size), &[tr], None);
        let a = f32::from(layout.ascent).abs();
        let d = f32::from(layout.descent).abs();
        (st.line_height.unwrap_or((a + d).ceil()), a, d)
    }
}

impl Shaper for GpuiShaper {
    fn shape(&self, job: Job) -> Arc<Shaped> {
        let mut chars: Vec<Ch> = Vec::new();
        for (k, (range, st)) in job.runs.iter().enumerate() {
            let text = &job.text[range.clone()];
            let mut first = true;
            for piece in text.split('\n') {
                if !first {
                    chars.push(Ch {
                        w: 0.0,
                        newline: true,
                        space: true,
                        glyphs: Vec::new(),
                        run: k,
                        ascent: 0.0,
                        descent: 0.0,
                    });
                }
                first = false;
                self.line(piece, st, k, &mut chars);
            }
        }
        let base = job.runs.first().map(|(_, s)| s.clone()).unwrap_or_else(|| {
            RunStyle::plain(Family::Sans, 14.0, Rgba(0xff, 0xff, 0xff, 0xff))
        });
        let (base_h, base_a, base_d) = self.metrics(&base);
        let max_w = job.wrap.unwrap_or(f32::INFINITY).max(1.0);
        // Rows as ranges of `chars`, and whether each ends at a newline.
        let mut rows: Vec<(usize, usize, bool)> = Vec::new();
        let (mut start, mut x, mut space) = (0, 0.0, None::<usize>);
        let mut i = 0;
        while i < chars.len() {
            let c = &chars[i];
            if c.newline {
                rows.push((start, i, true));
                (start, x, space) = (i + 1, 0.0, None);
                i += 1;
                continue;
            }
            if !job.ellipsis && job.wrap.is_some() && x + c.w > max_w && i > start && !c.space {
                match space.filter(|s| *s >= start) {
                    Some(s) => {
                        rows.push((start, s + 1, false));
                        start = s + 1;
                    }
                    None => {
                        rows.push((start, i, false));
                        start = i;
                    }
                }
                x = chars[start..i].iter().map(|c| c.w).sum();
                space = None;
                continue;
            }
            if c.space {
                space = Some(i);
            }
            x += c.w;
            i += 1;
        }
        rows.push((start, chars.len(), false));
        // One line cut short with "…".
        let mut ellipsis: Option<(f32, Vec<Ch>)> = None;
        if job.ellipsis
            && let Some(&(s, e, _)) = rows.first()
        {
            let total: f32 = chars[s..e].iter().map(|c| c.w).sum();
            if total > max_w {
                let st = job.runs.last().map(|(_, s)| s.clone()).unwrap_or(base.clone());
                let mut dots = Vec::new();
                self.line("…", &st, job.runs.len().saturating_sub(1), &mut dots);
                let dw: f32 = dots.iter().map(|c| c.w).sum();
                let mut w = 0.0;
                let mut cut = s;
                while cut < e && w + chars[cut].w + dw <= max_w {
                    w += chars[cut].w;
                    cut += 1;
                }
                rows = vec![(s, cut, false)];
                ellipsis = Some((w, dots));
            } else {
                rows.truncate(1);
            }
        }
        let mut laid = Laid::default();
        let mut out_rows = Vec::new();
        let mut y = 0.0;
        let mut char_index = 0;
        let mut width: f32 = 0.0;
        let widths: Vec<f32> = rows
            .iter()
            .map(|(s, e, _)| chars[*s..*e].iter().map(|c| c.w).sum::<f32>())
            .collect();
        let box_w = job.align_width().unwrap_or_else(|| widths.iter().copied().fold(0.0, f32::max));
        for (n, &(s, e, newline)) in rows.iter().enumerate() {
            let row = &chars[s..e];
            let (mut asc, mut desc, mut h) = (0.0f32, 0.0f32, 0.0f32);
            for c in row {
                let st = &job.runs[c.run].1;
                asc = asc.max(c.ascent);
                desc = desc.max(c.descent);
                h = h.max(st.line_height.unwrap_or(c.ascent + c.descent));
            }
            if row.is_empty() || h == 0.0 {
                (h, asc, desc) = (base_h.max(h), base_a.max(asc), base_d.max(desc));
            }
            let mut row_w = widths[n];
            if let Some((w, _)) = &ellipsis {
                row_w = *w;
            }
            let shift = match job.align {
                TextAlign::Left => 0.0,
                TextAlign::Center => ((box_w - row_w) / 2.0).max(0.0),
                TextAlign::Right => (box_w - row_w).max(0.0),
            };
            let baseline = y + (h - (asc + desc)) / 2.0 + asc;
            let mut xs = vec![shift];
            let mut x = shift;
            let paint = |c: &Ch, x: f32, laid: &mut Laid| {
                let st = &job.runs[c.run].1;
                for (dx, font, id, emoji) in &c.glyphs {
                    laid.glyphs.push(Glyph {
                        x: x + dx,
                        y: baseline,
                        font: *font,
                        id: *id,
                        size: st.size,
                        emoji: *emoji,
                        color: st.color,
                    });
                }
                let span = |top: f32, h: f32| Rect::from_xywh(x, top, c.w, h);
                if let Some(bg) = st.background {
                    laid.decos.push(Deco {
                        rect: span(y, h),
                        color: bg,
                        under: true,
                    });
                }
                if st.underline {
                    laid.decos.push(Deco {
                        rect: span(baseline + 1.5, 1.0),
                        color: st.color,
                        under: false,
                    });
                }
                if st.strike {
                    laid.decos.push(Deco {
                        rect: span(baseline - c.ascent * 0.3, 1.0),
                        color: st.color,
                        under: false,
                    });
                }
            };
            for c in row {
                paint(c, x, &mut laid);
                x += c.w;
                xs.push(x);
            }
            if let Some((_, dots)) = &ellipsis {
                for c in dots {
                    paint(c, x, &mut laid);
                    x += c.w;
                }
            }
            width = width.max(row_w);
            let r = Row {
                y,
                height: h,
                start: char_index,
                xs,
                newline,
            };
            char_index = r.end();
            out_rows.push(r);
            y += h;
        }
        // Underlines and strikes merge with their neighbours: no seams.
        laid.decos = merge(std::mem::take(&mut laid.decos));
        Arc::new(Shaped {
            text: job.text,
            size: vec2(width, y),
            rows: out_rows,
            handle: Box::new(laid),
        })
    }

    fn row_height(&self, family: &Family, size: f32) -> f32 {
        let st = RunStyle::plain(family.clone(), size, Rgba(0, 0, 0, 0xff));
        self.metrics(&st).0
    }

    fn mono_cell(&self, size: f32) -> (f32, f32) {
        let st = RunStyle::plain(Family::Mono, size, Rgba(0, 0, 0, 0xff));
        let mut chars = Vec::new();
        self.line("M", &st, 0, &mut chars);
        let w = chars.first().map_or(size * 0.6, |c| c.w);
        (w, self.metrics(&st).0)
    }
}

/// Boxes that touch on a line, of one color, as one.
fn merge(decos: Vec<Deco>) -> Vec<Deco> {
    let mut out: Vec<Deco> = Vec::with_capacity(decos.len());
    for d in decos {
        if let Some(last) = out.last_mut()
            && last.color == d.color
            && last.under == d.under
            && (last.rect.min.y - d.rect.min.y).abs() < 0.01
            && (last.rect.height() - d.rect.height()).abs() < 0.01
            && (last.rect.max.x - d.rect.min.x).abs() < 0.5
        {
            last.rect.max.x = d.rect.max.x;
            continue;
        }
        out.push(d);
    }
    out
}
