//! A field of dots whose grey drifts like slow currents: the GUI's version
//! of the TUI's new-chat canvas (a Braille cell is two columns of four
//! dots). Lua gives the look (`field = { colors, pitch, size, scale, speed,
//! fps }`); the noise runs here, because tens of thousands of dots a frame
//! are too many to make in Lua. The algorithm is the TUI library's, so the
//! two drift the same way: a lattice of seeded value noise, two octaves,
//! curled by slow sines, with the wall clock as time.

use std::cell::RefCell;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Mesh, Shape};

use crate::node::{Field, Rgba, Round};

struct Lattice {
    /// `grid[(iy << 8) | ix] = vals[(perm[ix] + iy) % 256]`, the corner the
    /// old per-sample `rem_euclid` would have read. Built once.
    grid: Box<[f32; 256 * 256]>,
    /// Kept so tests can check the table against the float remainder.
    #[cfg(test)]
    perm: [usize; 256],
    #[cfg(test)]
    vals: [f32; 256],
}

/// The TUI's generator: a fixed seed, so a reload picks the field up where
/// it was.
fn lattice() -> &'static Lattice {
    static L: OnceLock<Lattice> = OnceLock::new();
    L.get_or_init(|| {
        let mut state: u64 = 12345;
        let mut rand = |n: u64| {
            state = state * 16807 % 2147483647;
            state % n
        };
        let mut perm = [0usize; 256];
        let mut vals = [0f32; 256];
        for i in 0..256 {
            perm[i] = i;
            vals[i] = rand(65536) as f32 / 65535.0;
        }
        for i in (1..256).rev() {
            let j = rand(i as u64 + 1) as usize;
            perm.swap(i, j);
        }
        let mut grid = vec![0f32; 256 * 256].into_boxed_slice();
        for iy in 0..256 {
            for ix in 0..256 {
                grid[(iy << 8) | ix] = vals[(perm[ix] + iy) % 256];
            }
        }
        let grid: Box<[f32; 256 * 256]> = grid.try_into().unwrap_or_else(|_| unreachable!());
        Lattice {
            grid,
            #[cfg(test)]
            perm,
            #[cfg(test)]
            vals,
        }
    })
}

/// Value noise, periodic on 256. `ix` / `iy` are the floored coordinates;
/// masking the low 8 bits is `rem_euclid(256)` for every integer that fits
/// in `i64`, which these coordinates do, and it skips the slow float divide
/// the remainder used to do on the wall-clock-sized inputs.
#[inline(always)]
fn noise(grid: &[f32; 256 * 256], x: f64, y: f64) -> f64 {
    let x0 = x.floor();
    let y0 = y.floor();
    let u = fade(x - x0);
    let v = fade(y - y0);
    let ix = (x0 as i64 as u64) & 255;
    let iy = (y0 as i64 as u64) & 255;
    let ix1 = (ix + 1) & 255;
    let iy1 = (iy + 1) & 255;
    let a = f64::from(grid[((iy << 8) | ix) as usize]);
    let b = f64::from(grid[((iy << 8) | ix1) as usize]);
    let c = f64::from(grid[((iy1 << 8) | ix) as usize]);
    let d = f64::from(grid[((iy1 << 8) | ix1) as usize]);
    (a + (b - a) * u) * (1.0 - v) + (c + (d - c) * u) * v
}

#[inline(always)]
fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// Seconds, from the wall clock.
fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// A cell of the TUI is this many pixels: the field's coordinates are in
/// cells, so `scale` means what it does there.
const CELL: (f64, f64) = (8.0, 16.0);

fn color(c: Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied(c.0, c.1, c.2, c.3)
}

/// Whether a point is within the box's rounded corners.
fn inside(rect: egui::Rect, radius: f32, round: Round, x: f64, y: f64) -> bool {
    let r = f64::from(radius);
    let (left, right) = (f64::from(rect.min.x) + r, f64::from(rect.max.x) - r);
    let (top, bottom) = (f64::from(rect.min.y) + r, f64::from(rect.max.y) - r);
    let cx = x.clamp(left, right);
    let corner_y = if y < top {
        Some(top)
    } else if y > bottom && round == Round::All {
        Some(bottom)
    } else {
        None
    };
    match corner_y {
        Some(cy) if x < left || x > right => (x - cx).hypot(y - cy) <= r,
        _ => true,
    }
}

/// One dot per entry: `0` is clipped away, otherwise a 1-based palette index.
fn sample(rect: egui::Rect, f: &Field, radius: f32, t: f64) -> Vec<u8> {
    let levels = f.colors.len();
    let (pitch, cols, rows) = grid(rect, f);
    let scale = f64::from(f.scale);
    let curl_y: Vec<f64> = (0..cols)
        .map(|c| {
            let col = (c as f64 * pitch) / CELL.0;
            (col / 2.0 * 0.07 - t * 0.11).sin() * 1.6 - t * 0.09
        })
        .collect();
    let grid = &lattice().grid;
    let mut picked = Vec::with_capacity(cols * rows);
    // Same arithmetic as the TUI port, including the order of operations,
    // so a dot's grey does not move by a rounding error.
    for r in 0..rows {
        let y = r as f64 * pitch;
        let row = y / CELL.1;
        let curl_x = (row * 0.08 + t * 0.13).sin() * 1.6 + t * 0.12;
        let vy = row * 0.09 * scale;
        for (c, cy) in curl_y.iter().enumerate() {
            let col = (c as f64 * pitch) / CELL.0;
            let u = col * 0.045 * scale + curl_x;
            let v = vy + cy;
            let wave =
                noise(grid, u, v) * 0.75 + noise(grid, u * 2.1 - t * 0.08, v * 2.1 + 31.0) * 0.25;
            let level = (((wave - 0.15) / 0.7 * levels as f64).floor() as isize + 1)
                .clamp(1, levels as isize) as usize;
            let (cx, cyp) = (
                f64::from(rect.min.x) + c as f64 * pitch + pitch / 2.0,
                f64::from(rect.min.y) + y + pitch / 2.0,
            );
            let clipped =
                radius > 0.0 && f.round != Round::None && !inside(rect, radius, f.round, cx, cyp);
            picked.push(if clipped { 0 } else { level as u8 });
        }
    }
    picked
}

fn grid(rect: egui::Rect, f: &Field) -> (f64, usize, usize) {
    let pitch = f64::from(f.pitch.max(2.0));
    let cols = (f64::from(rect.width()) / pitch).ceil() as usize;
    let rows = (f64::from(rect.height()) / pitch).ceil() as usize;
    (pitch, cols, rows)
}

fn build_mesh(rect: egui::Rect, f: &Field, radius: f32, t: f64) -> Mesh {
    let _noise = crate::profile::Span::start("field-noise");
    let picked = sample(rect, f, radius, t);
    drop(_noise);
    let _mesh = crate::profile::Span::start("field-mesh");
    let (pitch, cols, rows) = grid(rect, f);
    let half = f64::from(f.size.max(0.5)) / 2.0;
    let palette: Vec<Color32> = f.colors.iter().map(|c| color(*c)).collect();
    let mut mesh = Mesh::default();
    mesh.reserve_triangles(cols * rows * 2);
    mesh.reserve_vertices(cols * rows * 4);
    let origin_x = f64::from(rect.min.x);
    let origin_y = f64::from(rect.min.y);
    for r in 0..rows {
        let y = r as f64 * pitch;
        let cyp = origin_y + y + pitch / 2.0;
        for c in 0..cols {
            let level = picked[r * cols + c];
            if level == 0 {
                continue;
            }
            let cx = origin_x + c as f64 * pitch + pitch / 2.0;
            let dot = egui::Rect::from_min_max(
                egui::pos2((cx - half) as f32, (cyp - half) as f32),
                egui::pos2((cx + half) as f32, (cyp + half) as f32),
            );
            mesh.add_colored_rect(dot.intersect(rect), palette[level as usize - 1]);
        }
    }
    mesh
}

struct Slot {
    rect: egui::Rect,
    pitch: f32,
    size: f32,
    scale: f32,
    speed: f32,
    fps: f32,
    radius: f32,
    round: Round,
    colors: Vec<Rgba>,
    built: Instant,
    mesh: Arc<Mesh>,
}

impl Slot {
    fn same(&self, rect: egui::Rect, f: &Field, radius: f32) -> bool {
        self.rect == rect
            && self.pitch == f.pitch
            && self.size == f.size
            && self.scale == f.scale
            && self.speed == f.speed
            && self.fps == f.fps
            && self.radius == radius
            && self.round == f.round
            && self.colors == f.colors
    }

    /// The field draws `fps` times a second (or once, when fps or speed is
    /// 0). A frame that arrives early, a keystroke on the new-chat screen,
    /// reuses this mesh. Two milliseconds of slack so the fps timer itself,
    /// which can fire a moment early, still gets a new frame.
    fn fresh(&self, now: Instant) -> bool {
        if self.fps <= 0.0 || self.speed == 0.0 {
            return true;
        }
        let interval = Duration::from_secs_f32(1.0 / self.fps);
        let slack = Duration::from_millis(2);
        now.duration_since(self.built) + slack < interval
    }
}

thread_local! {
    static CACHE: RefCell<Vec<Slot>> = const { RefCell::new(Vec::new()) };
}

fn cached(rect: egui::Rect, f: &Field, radius: f32) -> Option<Arc<Mesh>> {
    CACHE.with(|c| {
        let c = c.borrow();
        let slot = c.iter().find(|s| s.same(rect, f, radius))?;
        slot.fresh(Instant::now()).then(|| slot.mesh.clone())
    })
}

fn store(rect: egui::Rect, f: &Field, radius: f32, mesh: Arc<Mesh>) {
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        let slot = Slot {
            rect,
            pitch: f.pitch,
            size: f.size,
            scale: f.scale,
            speed: f.speed,
            fps: f.fps,
            radius,
            round: f.round,
            colors: f.colors.clone(),
            built: Instant::now(),
            mesh,
        };
        if let Some(prev) = c.iter_mut().find(|s| s.same(rect, f, radius)) {
            *prev = slot;
            return;
        }
        if c.len() >= 8 {
            let oldest = c
                .iter()
                .enumerate()
                .min_by_key(|(_, s)| s.built)
                .map(|(i, _)| i)
                .unwrap_or(0);
            c[oldest] = slot;
        } else {
            c.push(slot);
        }
    });
}

/// Paint the field in `rect`; true while it wants another frame.
pub fn paint(painter: &egui::Painter, rect: egui::Rect, f: &Field, radius: f32) -> bool {
    let _span = crate::profile::Span::start("field");
    if f.colors.is_empty() || rect.width() < 1.0 || rect.height() < 1.0 {
        return false;
    }
    let mesh = match cached(rect, f, radius) {
        Some(mesh) => mesh,
        None => {
            let mesh = Arc::new(build_mesh(rect, f, radius, now() * f64::from(f.speed)));
            store(rect, f, radius, mesh.clone());
            mesh
        }
    };
    painter.add(Shape::Mesh(mesh));
    f.fps > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn welcome() -> Field {
        Field {
            colors: (0..10)
                .map(|i| Rgba(7 + i * 2, 7 + i * 2, 7 + i * 2, 255))
                .collect(),
            pitch: 5.0,
            size: 2.2,
            scale: 1.0,
            speed: 1.0,
            fps: 20.0,
            round: Round::Top,
        }
    }

    /// The noise as it was before the lattice table: float `rem_euclid` on
    /// every corner. The fast path must return the same bits.
    fn noise_ref(l: &Lattice, x: f64, y: f64) -> f64 {
        let (ix, iy) = (x.floor(), y.floor());
        let (u, v) = (x - ix, y - iy);
        let fade = |t: f64| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
        let (u, v) = (fade(u), fade(v));
        let at = |n: f64| n.rem_euclid(256.0) as usize;
        let (px, px1) = (l.perm[at(ix)], l.perm[at(ix + 1.0)]);
        let val = |p: usize, d: f64| f64::from(l.vals[(p + at(iy + d)) % 256]);
        let (a, b) = (val(px, 0.0), val(px1, 0.0));
        let (c, d) = (val(px, 1.0), val(px1, 1.0));
        (a + (b - a) * u) * (1.0 - v) + (c + (d - c) * u) * v
    }

    #[test]
    fn noise_matches_the_float_remainder() {
        let l = lattice();
        let samples = [
            0.0, 0.3, 1.0, 255.2, 256.0, 1000.7, -0.3, -1.7, -255.4, -256.0, -1000.2, 2.076e8,
            -1.53e8, 31.0, -0.0,
        ];
        for &x in &samples {
            for &y in &samples {
                let a = noise(&l.grid, x, y);
                let b = noise_ref(l, x, y);
                assert_eq!(a.to_bits(), b.to_bits(), "noise({x}, {y})");
            }
        }
    }

    #[test]
    fn dots_match_across_a_rounded_field() {
        let f = welcome();
        let rect = egui::Rect::from_min_size(egui::pos2(12.0, 8.0), egui::vec2(640.0, 400.0));
        let t = 1_700_000_123.5;
        let fast = sample(rect, &f, 16.0, t);
        let slow = sample_ref(rect, &f, 16.0, t);
        assert_eq!(fast.len(), slow.len());
        for (i, (a, b)) in fast.iter().zip(slow.iter()).enumerate() {
            assert_eq!(a, b, "dot {i}");
        }
        assert!(fast.contains(&0), "rounded corners clip");
        assert!(fast.iter().any(|d| *d > 1), "more than one grey");
    }

    fn sample_ref(rect: egui::Rect, f: &Field, radius: f32, t: f64) -> Vec<u8> {
        let levels = f.colors.len();
        let pitch = f64::from(f.pitch.max(2.0));
        let cols = (f64::from(rect.width()) / pitch).ceil() as usize;
        let rows = (f64::from(rect.height()) / pitch).ceil() as usize;
        let scale = f64::from(f.scale);
        let curl_y: Vec<f64> = (0..cols)
            .map(|c| {
                let col = (c as f64 * pitch) / CELL.0;
                (col / 2.0 * 0.07 - t * 0.11).sin() * 1.6 - t * 0.09
            })
            .collect();
        let l = lattice();
        let mut picked = Vec::with_capacity(cols * rows);
        for r in 0..rows {
            let y = r as f64 * pitch;
            let row = y / CELL.1;
            let curl_x = (row * 0.08 + t * 0.13).sin() * 1.6 + t * 0.12;
            let vy = row * 0.09 * scale;
            for (c, cy) in curl_y.iter().enumerate() {
                let col = (c as f64 * pitch) / CELL.0;
                let u = col * 0.045 * scale + curl_x;
                let v = vy + cy;
                let wave = noise_ref(l, u, v) * 0.75
                    + noise_ref(l, u * 2.1 - t * 0.08, v * 2.1 + 31.0) * 0.25;
                let level = (((wave - 0.15) / 0.7 * levels as f64).floor() as isize + 1)
                    .clamp(1, levels as isize) as usize;
                let (cx, cyp) = (
                    f64::from(rect.min.x) + c as f64 * pitch + pitch / 2.0,
                    f64::from(rect.min.y) + y + pitch / 2.0,
                );
                let clipped = radius > 0.0
                    && f.round != Round::None
                    && !inside(rect, radius, f.round, cx, cyp);
                picked.push(if clipped { 0 } else { level as u8 });
            }
        }
        picked
    }
}
