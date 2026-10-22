//! Per-area view state (scroll position for the chat and the prompt), and
//! the layout solver: `bone.ui.layout` in an area of character units.

use crate::model::{LayoutNode, Size};

pub type WindowId = usize;
pub type BufferId = usize;

#[derive(Debug, Clone)]
pub struct Window {
    /// First visible row.
    pub top: usize,
    /// Keep the view pinned to the end as content grows.
    pub follow: bool,
}

impl Default for Window {
    fn default() -> Self {
        Window {
            top: 0,
            follow: true,
        }
    }
}

/// An area in character units: terminal cells in the TUI, columns of the
/// monospace font's advance width and rows of its line height in the GUI.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn right(&self) -> u16 {
        self.x.saturating_add(self.width)
    }

    pub const fn bottom(&self) -> u16 {
        self.y.saturating_add(self.height)
    }

    pub const fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

/// Where the layout put things.
#[derive(Debug, Default)]
pub struct Plan {
    /// Every leaf and its area.
    pub leaves: Vec<(String, Rect)>,
    /// Regions to paint at their final dimensions.
    pub regions: Vec<(String, Rect)>,
    /// Column separators: x, the split's area, the symbol, and the path
    /// (1-based child indexes) to the column after it.
    pub seps: Vec<(u16, Rect, String, Vec<usize>)>,
}

/// The leaves the frontend draws itself; a leaf named anything else is a
/// Lua region. Built-ins other than `message` take `:<session id>` for
/// that session's chat.
pub const BUILTIN: [&str; 5] = ["chat", "prompt", "divider", "statusline", "message"];

/// What a leaf is: `panel`, a built-in, or (anything else) a region.
pub fn kind(name: &str) -> &str {
    name.split(':').next().unwrap_or(name)
}

pub fn is_region(name: &str) -> bool {
    !BUILTIN.contains(&kind(name)) && kind(name) != "panel"
}

/// What the layout asks the frontend while it sizes leaves.
pub trait Measure {
    /// Panel `id` is hidden or missing.
    fn panel_hidden(&self, id: &str) -> bool;
    /// Prompt leaf `name`'s natural height at `width`, its border and
    /// padding included.
    fn prompt_height(&mut self, name: &str, width: u16) -> u16;
    /// Whether `bone.ui[name]` draws the statusline or divider row.
    fn line_defined(&self, name: &str) -> bool;
    /// Rows the message line takes in `area`.
    fn message_rows(&self, area: Rect) -> u16;
    /// Panel `id`'s natural size in `width` x `height` (rows of a stack, or
    /// columns of a row).
    fn panel_size(&mut self, id: &str, width: u16, height: u16, rows: bool) -> u16;
    /// Region `name`'s natural size, or `None` when it is not defined.
    fn region_size(&mut self, name: &str, width: u16, height: u16, rows: bool) -> Option<u16>;
}

/// Panel-only subtrees disappear when every referenced panel is hidden or missing.
fn empty_panels(m: &impl Measure, node: &LayoutNode) -> bool {
    match node {
        LayoutNode::Leaf { name, .. } => name
            .strip_prefix("panel:")
            .is_some_and(|id| m.panel_hidden(id)),
        LayoutNode::Split { children, .. } => children.iter().all(|c| empty_panels(m, c)),
    }
}

/// Lay `node` out in `area`. In a split, fixed and percent sizes come first,
/// then natural sizes (the prompt, one-row statusline and divider, the
/// message line, then regions, which leave a filling sibling at least 3
/// rows), and filling children share the rest.
pub fn place(
    m: &mut impl Measure,
    node: &LayoutNode,
    area: Rect,
    plan: &mut Plan,
    path: &mut Vec<usize>,
) {
    let LayoutNode::Split {
        rows,
        sep,
        children,
        ..
    } = node
    else {
        if let LayoutNode::Leaf { name, .. } = node {
            plan.leaves.push((name.clone(), area));
            if is_region(name) {
                plan.regions.push((name.clone(), area));
            }
        }
        return;
    };
    let rows = *rows;
    let n = children.len();
    // Separators go only between columns that have something in them; room
    // for the most there could be is kept back while sizing regions.
    let sep = sep.as_ref().filter(|_| !rows);
    let most_seps = if sep.is_some() {
        n.saturating_sub(1) as u16
    } else {
        0
    };
    let total = if rows { area.height } else { area.width };
    let cross = if rows { area.width } else { area.height };
    let mut sizes: Vec<Option<u16>> = vec![None; n];
    fn leaf(c: &LayoutNode) -> Option<&str> {
        match c {
            LayoutNode::Leaf { name, .. } => Some(name),
            _ => None,
        }
    }
    for (i, c) in children.iter().enumerate() {
        sizes[i] = match (c.size(), leaf(c).map(kind)) {
            _ if empty_panels(m, c) => Some(0),
            (Size::Cells(v), _) => Some(v),
            (Size::Percent(p), _) => Some((u32::from(total) * u32::from(p) / 100) as u16),
            (Size::Auto, Some("prompt")) => {
                let width = if rows { cross } else { total };
                let h = m.prompt_height(leaf(c).unwrap_or("prompt"), width);
                Some(if rows {
                    h.min(total.saturating_sub(1))
                } else {
                    total / 2
                })
            }
            (Size::Auto, Some(name @ ("divider" | "statusline"))) => {
                Some(u16::from(m.line_defined(name)))
            }
            (Size::Auto, Some("message")) => Some(m.message_rows(area)),
            _ => None,
        };
    }
    let fills = children
        .iter()
        .enumerate()
        .filter(|(i, c)| {
            sizes[*i].is_none()
                && !(c.size() == Size::Auto && leaf(c).is_some_and(|n| !BUILTIN.contains(&kind(n))))
        })
        .count();
    let mut used = sizes
        .iter()
        .flatten()
        .fold(0u16, |sum, n| sum.saturating_add(*n));
    let keep = if fills > 0 && rows { 3 } else { 0 };
    let mut spare = total.saturating_sub(used.saturating_add(keep).saturating_add(most_seps));
    for (i, c) in children.iter().enumerate() {
        if sizes[i].is_some() || c.size() != Size::Auto {
            continue;
        }
        let Some(name) = leaf(c).filter(|n| !BUILTIN.contains(&kind(n))) else {
            continue;
        };
        let (avail_w, avail_h) = if rows { (cross, spare) } else { (total, cross) };
        sizes[i] = Some(if let Some(id) = name.strip_prefix("panel:") {
            m.panel_size(id, avail_w, avail_h, rows).min(spare)
        } else {
            match m.region_size(name, avail_w, avail_h, rows) {
                Some(size) => size.min(spare),
                None => 0,
            }
        });
        let size = sizes[i].unwrap_or(0);
        spare = spare.saturating_sub(size);
        used = used.saturating_add(size);
    }
    let fill_count = sizes.iter().filter(|s| s.is_none()).count() as u16;
    let shown = sizes.iter().filter(|s| s.is_none_or(|v| v > 0)).count() as u16;
    let seps = if sep.is_some() {
        shown.saturating_sub(1)
    } else {
        0
    };
    let left = total.saturating_sub(used.saturating_add(seps));
    let mut extra = if fill_count > 0 { left % fill_count } else { 0 };
    let mut at = if rows { area.y } else { area.x };
    let end = if rows { area.bottom() } else { area.right() };
    let mut shown_before = false;
    for (i, c) in children.iter().enumerate() {
        let mut size = sizes[i].unwrap_or_else(|| {
            let share = left / fill_count.max(1) + u16::from(extra > 0);
            extra = extra.saturating_sub(1);
            share
        });
        if let Some(sep) = sep
            && size > 0
            && shown_before
            && at < end
        {
            let mut to = path.clone();
            to.push(i + 1);
            plan.seps.push((at, area, sep.clone(), to));
            at += 1;
        }
        shown_before |= size > 0;
        size = size.min(end.saturating_sub(at));
        let r = if rows {
            Rect {
                y: at,
                height: size,
                ..area
            }
        } else {
            Rect {
                x: at,
                width: size,
                ..area
            }
        };
        path.push(i + 1);
        place(m, c, r, plan, path);
        path.pop();
        at += size;
    }
}
