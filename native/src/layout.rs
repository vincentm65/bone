//! Zed-style pane grid: a tree of splits whose leaves are pane ids.
//!
//! Pure data, no egui. The app maps each [`PaneId`] to its own tab list.

pub type PaneId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Children sit side by side.
    Horizontal,
    /// Children are stacked top to bottom.
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// Rectangle in the unit square.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct URect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl URect {
    pub const UNIT: URect = URect {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Leaf(PaneId),
    Split {
        axis: Axis,
        /// `(fraction, child)`; fractions sum to 1.
        children: Vec<(f32, Node)>,
    },
}

/// Smallest fraction a child may shrink to when dragging a divider.
pub const MIN_FRACTION: f32 = 0.1;

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub root: Node,
    pub focused: PaneId,
    next_id: PaneId,
}

impl Default for Layout {
    fn default() -> Self {
        Self::new()
    }
}

impl Layout {
    pub fn new() -> Self {
        Self {
            root: Node::Leaf(0),
            focused: 0,
            next_id: 1,
        }
    }

    /// Pane ids in reading order.
    pub fn leaves(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        collect(&self.root, &mut out);
        out
    }

    pub fn contains(&self, id: PaneId) -> bool {
        self.leaves().contains(&id)
    }

    /// Split `target`, placing a new pane after (or before) it. Returns the new
    /// pane id, which becomes focused. `None` if `target` doesn't exist.
    pub fn split(&mut self, target: PaneId, axis: Axis, before: bool) -> Option<PaneId> {
        if !self.contains(target) {
            return None;
        }
        let new = self.next_id;
        self.next_id += 1;
        split_node(&mut self.root, target, new, axis, before);
        self.focused = new;
        Some(new)
    }

    /// Remove a pane, giving its space to a sibling. The last pane can't be
    /// closed. Focus moves to a neighbouring pane if the closed one had it.
    pub fn close(&mut self, id: PaneId) -> bool {
        let leaves = self.leaves();
        if leaves.len() <= 1 {
            return false;
        }
        let Some(pos) = leaves.iter().position(|l| *l == id) else {
            return false;
        };
        remove_node(&mut self.root, id);
        if let Node::Split { children, .. } = &mut self.root
            && children.len() == 1
        {
            self.root = children.remove(0).1;
        }
        if self.focused == id {
            let rest = self.leaves();
            self.focused = rest[pos.saturating_sub(1).min(rest.len() - 1)];
        }
        true
    }

    /// Leaf rectangles in the unit square.
    pub fn rects(&self) -> Vec<(PaneId, URect)> {
        let mut out = Vec::new();
        rects_of(&self.root, URect::UNIT, &mut out);
        out
    }

    /// Nearest pane in `dir` from `from`, judged by geometry.
    pub fn neighbor(&self, from: PaneId, dir: Dir) -> Option<PaneId> {
        const EPS: f32 = 1e-4;
        let rects = self.rects();
        let cur = rects.iter().find(|(id, _)| *id == from)?.1;
        let (cx, cy) = (cur.x + cur.w / 2.0, cur.y + cur.h / 2.0);
        rects
            .iter()
            .filter(|(id, _)| *id != from)
            .filter_map(|(id, r)| {
                let (overlap, gap) = match dir {
                    Dir::Left => (span(cur.y, cur.h, r.y, r.h), cur.x - (r.x + r.w)),
                    Dir::Right => (span(cur.y, cur.h, r.y, r.h), r.x - (cur.x + cur.w)),
                    Dir::Up => (span(cur.x, cur.w, r.x, r.w), cur.y - (r.y + r.h)),
                    Dir::Down => (span(cur.x, cur.w, r.x, r.w), r.y - (cur.y + cur.h)),
                };
                if overlap <= EPS || gap < -EPS {
                    return None;
                }
                let d = (r.x + r.w / 2.0 - cx).abs() + (r.y + r.h / 2.0 - cy).abs();
                Some((*id, gap, d))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1).then(a.2.total_cmp(&b.2)))
            .map(|(id, ..)| id)
    }

    /// Move focus in `dir`. Returns whether focus changed.
    pub fn focus_dir(&mut self, dir: Dir) -> bool {
        match self.neighbor(self.focused, dir) {
            Some(id) => {
                self.focused = id;
                true
            }
            None => false,
        }
    }

    /// Drag the divider after child `index` of the split at `path` (child
    /// indices from the root) by `delta` (fraction of the split's size).
    pub fn resize(&mut self, path: &[usize], index: usize, delta: f32) {
        let mut node = &mut self.root;
        for i in path {
            match node {
                Node::Split { children, .. } if *i < children.len() => node = &mut children[*i].1,
                _ => return,
            }
        }
        let Node::Split { children, .. } = node else {
            return;
        };
        if index + 1 >= children.len() {
            return;
        }
        let total = children[index].0 + children[index + 1].0;
        let min = MIN_FRACTION.min(total / 2.0);
        let a = (children[index].0 + delta).clamp(min, total - min);
        children[index].0 = a;
        children[index + 1].0 = total - a;
    }
}

fn span(a: f32, alen: f32, b: f32, blen: f32) -> f32 {
    (a + alen).min(b + blen) - a.max(b)
}

fn collect(node: &Node, out: &mut Vec<PaneId>) {
    match node {
        Node::Leaf(id) => out.push(*id),
        Node::Split { children, .. } => children.iter().for_each(|(_, c)| collect(c, out)),
    }
}

fn split_node(node: &mut Node, target: PaneId, new: PaneId, axis: Axis, before: bool) -> bool {
    match node {
        Node::Leaf(id) if *id == target => {
            let pair = if before { [new, target] } else { [target, new] };
            *node = Node::Split {
                axis,
                children: pair.iter().map(|id| (0.5, Node::Leaf(*id))).collect(),
            };
            true
        }
        Node::Leaf(_) => false,
        Node::Split {
            axis: node_axis,
            children,
        } => {
            // Same-axis split of a direct leaf child: insert a sibling instead of nesting.
            if *node_axis == axis
                && let Some(i) = children
                    .iter()
                    .position(|(_, c)| matches!(c, Node::Leaf(id) if *id == target))
            {
                let half = children[i].0 / 2.0;
                children[i].0 = half;
                let at = if before { i } else { i + 1 };
                children.insert(at, (half, Node::Leaf(new)));
                return true;
            }
            children
                .iter_mut()
                .any(|(_, c)| split_node(c, target, new, axis, before))
        }
    }
}

fn remove_node(node: &mut Node, id: PaneId) -> bool {
    let Node::Split { children, .. } = node else {
        return false;
    };
    if let Some(i) = children
        .iter()
        .position(|(_, c)| matches!(c, Node::Leaf(l) if *l == id))
    {
        let (freed, _) = children.remove(i);
        // Give the space to the previous sibling (or the next when first).
        let to = i.saturating_sub(1);
        children[to].0 += freed;
        return true;
    }
    for (_, child) in children.iter_mut() {
        if remove_node(child, id) {
            // Collapse a split left with a single child.
            if let Node::Split {
                children: inner, ..
            } = child
                && inner.len() == 1
            {
                *child = inner.remove(0).1;
            }
            return true;
        }
    }
    false
}

fn rects_of(node: &Node, r: URect, out: &mut Vec<(PaneId, URect)>) {
    match node {
        Node::Leaf(id) => out.push((*id, r)),
        Node::Split { axis, children } => {
            let mut off = 0.0;
            for (f, c) in children {
                let cr = match axis {
                    Axis::Horizontal => URect {
                        x: r.x + off * r.w,
                        w: f * r.w,
                        ..r
                    },
                    Axis::Vertical => URect {
                        y: r.y + off * r.h,
                        h: f * r.h,
                        ..r
                    },
                };
                rects_of(c, cr, out);
                off += f;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sums_to_one(node: &Node) -> bool {
        match node {
            Node::Leaf(_) => true,
            Node::Split { children, .. } => {
                (children.iter().map(|(f, _)| f).sum::<f32>() - 1.0).abs() < 1e-4
                    && children.iter().all(|(_, c)| sums_to_one(c))
            }
        }
    }

    #[test]
    fn split_adds_pane_and_focuses_it() {
        let mut l = Layout::new();
        let b = l.split(0, Axis::Horizontal, false).unwrap();
        assert_eq!(l.leaves(), vec![0, b]);
        assert_eq!(l.focused, b);
        assert!(sums_to_one(&l.root));
    }

    #[test]
    fn split_before_places_new_pane_first() {
        let mut l = Layout::new();
        let b = l.split(0, Axis::Horizontal, true).unwrap();
        assert_eq!(l.leaves(), vec![b, 0]);
    }

    #[test]
    fn same_axis_split_stays_flat() {
        let mut l = Layout::new();
        l.split(0, Axis::Horizontal, false);
        l.split(0, Axis::Horizontal, false);
        let Node::Split { children, .. } = &l.root else {
            panic!()
        };
        assert_eq!(children.len(), 3);
        assert!(sums_to_one(&l.root));
    }

    #[test]
    fn cross_axis_split_nests() {
        let mut l = Layout::new();
        let b = l.split(0, Axis::Horizontal, false).unwrap();
        let c = l.split(b, Axis::Vertical, false).unwrap();
        assert_eq!(l.leaves(), vec![0, b, c]);
        assert!(sums_to_one(&l.root));
    }

    #[test]
    fn split_unknown_pane_is_none() {
        assert!(Layout::new().split(9, Axis::Vertical, false).is_none());
    }

    #[test]
    fn close_collapses_and_redistributes() {
        let mut l = Layout::new();
        let b = l.split(0, Axis::Horizontal, false).unwrap();
        let c = l.split(b, Axis::Vertical, false).unwrap();
        assert!(l.close(c));
        assert!(sums_to_one(&l.root));
        assert!(l.close(b));
        assert_eq!(l.root, Node::Leaf(0));
        assert_eq!(l.focused, 0);
    }

    #[test]
    fn last_pane_cannot_close() {
        let mut l = Layout::new();
        assert!(!l.close(0));
        assert!(!l.close(5));
    }

    #[test]
    fn close_moves_focus_to_neighbour() {
        let mut l = Layout::new();
        let b = l.split(0, Axis::Horizontal, false).unwrap();
        l.close(b);
        assert_eq!(l.focused, 0);
    }

    #[test]
    fn focus_moves_by_geometry() {
        let mut l = Layout::new();
        let b = l.split(0, Axis::Horizontal, false).unwrap();
        let c = l.split(b, Axis::Vertical, false).unwrap();
        // 0 | b over c
        l.focused = 0;
        assert!(l.focus_dir(Dir::Right));
        assert_eq!(l.focused, b);
        assert!(l.focus_dir(Dir::Down));
        assert_eq!(l.focused, c);
        assert!(!l.focus_dir(Dir::Down));
        assert!(l.focus_dir(Dir::Left));
        assert_eq!(l.focused, 0);
        assert!(!l.focus_dir(Dir::Left));
        assert!(!l.focus_dir(Dir::Up));
    }

    #[test]
    fn resize_clamps_and_keeps_sum() {
        let mut l = Layout::new();
        l.split(0, Axis::Horizontal, false);
        l.resize(&[], 0, 0.25);
        let Node::Split { children, .. } = &l.root else {
            panic!()
        };
        assert!((children[0].0 - 0.75).abs() < 1e-4);
        l.resize(&[], 0, 5.0);
        let Node::Split { children, .. } = &l.root else {
            panic!()
        };
        assert!((children[1].0 - MIN_FRACTION).abs() < 1e-4);
        assert!(sums_to_one(&l.root));
    }

    #[test]
    fn rects_tile_the_unit_square() {
        let mut l = Layout::new();
        let b = l.split(0, Axis::Horizontal, false).unwrap();
        l.split(b, Axis::Vertical, false);
        let area: f32 = l.rects().iter().map(|(_, r)| r.w * r.h).sum();
        assert!((area - 1.0).abs() < 1e-4);
    }
}
