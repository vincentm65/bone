//! Frontend-only ownership tree. IDs never refer to daemon/session internals.
use bone_protocol::view::PanelSlot;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub type Id = u64;

/// Client-local arrangement for daemon-owned panels. Panel ids are opaque daemon
/// ids; entries that are not present in the current view are discarded on sync.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PanelLayout {
    #[serde(default)]
    pub panels: Vec<PanelEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PanelEntry {
    pub id: String,
    pub slot: PanelSlot,
    #[serde(default)]
    pub order: i32,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub size: Option<f32>,
}

impl Default for PanelLayout {
    fn default() -> Self {
        Self { panels: Vec::new() }
    }
}

impl PanelLayout {
    /// Reconcile persisted entries with the daemon's current view, preserving
    /// local ordering/visibility while giving new panels their declared slot.
    pub fn sync<I>(&mut self, current: I)
    where
        I: IntoIterator<Item = (String, PanelSlot, i32)>,
    {
        let current: Vec<_> = current.into_iter().collect();
        self.panels
            .retain(|entry| current.iter().any(|(id, _, _)| id == &entry.id));
        for (id, slot, order) in current {
            if !self.panels.iter().any(|entry| entry.id == id) {
                self.panels.push(PanelEntry {
                    id,
                    slot,
                    order,
                    hidden: false,
                    size: None,
                });
            }
        }
        self.panels
            .sort_by_key(|entry| (entry.slot as u8, entry.order));
    }

    pub fn entry(&self, id: &str) -> Option<&PanelEntry> {
        self.panels.iter().find(|entry| entry.id == id)
    }

    pub fn entry_mut(&mut self, id: &str) -> Option<&mut PanelEntry> {
        self.panels.iter_mut().find(|entry| entry.id == id)
    }

    pub fn set_slot(&mut self, id: &str, slot: PanelSlot) {
        if let Some(entry) = self.entry_mut(id) {
            entry.slot = slot;
        }
    }

    pub fn reorder(&mut self, id: &str, direction: i32) {
        let Some(index) = self.panels.iter().position(|entry| entry.id == id) else {
            return;
        };
        let slot = self.panels[index].slot;
        let peers: Vec<_> = self
            .panels
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.slot == slot)
            .map(|(index, _)| index)
            .collect();
        let Some(peer) = peers.iter().position(|peer_index| *peer_index == index) else {
            return;
        };
        let target = if direction < 0 {
            peer.checked_sub(1)
        } else {
            (peer + 1 < peers.len()).then_some(peer + 1)
        };
        let Some(target) = target else { return };
        self.panels.swap(peers[peer], peers[target]);
        for (order, entry) in self
            .panels
            .iter_mut()
            .filter(|entry| entry.slot == slot)
            .enumerate()
        {
            entry.order = order as i32;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pane {
    pub id: Id,
    pub tabs: Vec<Id>,
    pub active: Option<Id>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Node {
    Pane(Pane),
    Split {
        id: Id,
        axis: Axis,
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

impl Node {
    pub fn panes(&self) -> Vec<&Pane> {
        match self {
            Self::Pane(pane) => vec![pane],
            Self::Split { first, second, .. } => {
                let mut panes = first.panes();
                panes.extend(second.panes());
                panes
            }
        }
    }

    fn contains_id(&self, target: Id) -> bool {
        match self {
            Self::Pane(pane) => pane.id == target,
            Self::Split {
                id, first, second, ..
            } => *id == target || first.contains_id(target) || second.contains_id(target),
        }
    }

    fn pane_mut(&mut self, id: Id) -> Option<&mut Pane> {
        match self {
            Self::Pane(pane) => (pane.id == id).then_some(pane),
            Self::Split { first, second, .. } => first.pane_mut(id).or_else(|| second.pane_mut(id)),
        }
    }

    fn node_mut(&mut self, pane: Id) -> Option<&mut Node> {
        match self {
            Self::Pane(p) if p.id == pane => Some(self),
            Self::Pane(_) => None,
            Self::Split { first, second, .. } => {
                first.node_mut(pane).or_else(|| second.node_mut(pane))
            }
        }
    }

    /// Remove just this leaf and promote its sibling; other empty leaves are intentional.
    fn remove_pane(&mut self, id: Id) -> bool {
        match self {
            Self::Pane(_) => false,
            Self::Split { first, second, .. } => {
                if matches!(&**first, Self::Pane(pane) if pane.id == id) {
                    *self = (**second).clone();
                    true
                } else if matches!(&**second, Self::Pane(pane) if pane.id == id) {
                    *self = (**first).clone();
                    true
                } else {
                    first.remove_pane(id) || second.remove_pane(id)
                }
            }
        }
    }

    fn set_ratio(&mut self, target: Id, value: f32) -> bool {
        if let Self::Split {
            id,
            ratio,
            first,
            second,
            ..
        } = self
        {
            if *id == target {
                let value = finite_clamp(value, 0.1, 0.9, 0.5);
                let changed = *ratio != value;
                *ratio = value;
                changed
            } else {
                first.set_ratio(target, value) || second.set_ratio(target, value)
            }
        } else {
            false
        }
    }

    fn remap_tabs(&mut self, map: &impl Fn(Id) -> Option<Id>) {
        match self {
            Self::Pane(pane) => {
                pane.tabs = pane.tabs.iter().filter_map(|id| map(*id)).collect();
                pane.active = pane.active.and_then(map);
            }
            Self::Split { first, second, .. } => {
                first.remap_tabs(map);
                second.remap_tabs(map);
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub id: Id,
    pub root: Node,
    pub focused_pane: Id,
    pub size: [f32; 2],
    pub position: Option<[f32; 2]>,
}

impl Window {
    fn empty(id: Id, pane: Id) -> Self {
        Self {
            id,
            root: Node::Pane(Pane {
                id: pane,
                tabs: Vec::new(),
                active: None,
            }),
            focused_pane: pane,
            size: [1000.0, 720.0],
            position: None,
        }
    }
    fn repair_focus(&mut self) {
        if !self
            .root
            .panes()
            .iter()
            .any(|pane| pane.id == self.focused_pane)
        {
            self.focused_pane = self.root.panes()[0].id;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub windows: Vec<Window>,
    pub active_window: Id,
    #[serde(default)]
    next_id: Id,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            windows: vec![Window::empty(0, 1)],
            active_window: 0,
            next_id: 2,
        }
    }
}

impl Workspace {
    fn alloc(&mut self) -> Id {
        // Persisted IDs may occupy a sparse range all the way up to u64::MAX.
        while self.next_id == 0
            || self
                .windows
                .iter()
                .any(|w| w.id == self.next_id || w.root.contains_id(self.next_id))
        {
            self.next_id = self.next_id.wrapping_add(1);
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }
    pub fn window(&self, id: Id) -> Option<&Window> {
        self.windows.iter().find(|w| w.id == id)
    }
    pub fn window_mut(&mut self, id: Id) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.id == id)
    }
    pub fn pane(&self, id: Id) -> Option<&Pane> {
        self.windows
            .iter()
            .flat_map(|w| w.root.panes())
            .find(|p| p.id == id)
    }
    pub fn pane_mut(&mut self, id: Id) -> Option<&mut Pane> {
        self.windows.iter_mut().find_map(|w| w.root.pane_mut(id))
    }
    pub fn pane_window(&self, pane: Id) -> Option<Id> {
        self.windows
            .iter()
            .find(|w| w.root.panes().iter().any(|p| p.id == pane))
            .map(|w| w.id)
    }
    pub fn tab_location(&self, tab: Id) -> Option<(Id, Id)> {
        self.windows.iter().find_map(|w| {
            w.root
                .panes()
                .into_iter()
                .find(|p| p.tabs.contains(&tab))
                .map(|p| (w.id, p.id))
        })
    }
    pub fn focused_pane(&self, window: Id) -> Option<Id> {
        self.window(window).map(|w| w.focused_pane)
    }
    pub fn active_tab(&self, window: Id) -> Option<Id> {
        self.focused_pane(window)
            .and_then(|id| self.pane(id))
            .and_then(|p| p.active)
    }

    pub fn add_tab(&mut self, window: Id, tab: Id) {
        if self.focus_tab(tab).is_some() {
            return;
        }
        let pane = self
            .focused_pane(window)
            .or_else(|| self.focused_pane(0))
            .unwrap();
        let p = self.pane_mut(pane).unwrap();
        p.tabs.push(tab);
        p.active = Some(tab);
        self.focus_pane(pane);
    }
    pub fn focus_tab(&mut self, tab: Id) -> Option<Id> {
        let (window, pane) = self.tab_location(tab)?;
        self.pane_mut(pane)?.active = Some(tab);
        self.focus_pane(pane);
        Some(window)
    }
    pub fn focus_pane(&mut self, pane: Id) {
        if let Some(window) = self.pane_window(pane) {
            self.window_mut(window).unwrap().focused_pane = pane;
            self.active_window = window;
        }
    }
    fn take_tab(&mut self, tab: Id, collapse: bool) -> Option<(Id, Id, usize)> {
        let (window, pane) = self.tab_location(tab)?;
        let source = self.pane_mut(pane)?;
        let index = source.tabs.iter().position(|id| *id == tab)?;
        source.tabs.remove(index);
        if source.active == Some(tab) {
            source.active = source
                .tabs
                .get(index.min(source.tabs.len().saturating_sub(1)))
                .copied();
        }
        if collapse && source.tabs.is_empty() {
            let window = self.window_mut(window).unwrap();
            window.root.remove_pane(pane);
            window.repair_focus();
        }
        Some((window, pane, index))
    }
    /// Index is an insertion boundary in the target's pre-move tab order.
    pub fn move_tab(&mut self, tab: Id, target: Id, index: usize) -> bool {
        if self.pane(target).is_none() || self.tab_location(tab).is_none() {
            return false;
        }
        let source_pane = self.tab_location(tab).unwrap().1;
        let (_, _, old_index) = self.take_tab(tab, source_pane != target).unwrap();
        let index = if source_pane == target && old_index < index {
            index.saturating_sub(1)
        } else {
            index
        };
        let target_pane = self.pane_mut(target).unwrap();
        target_pane
            .tabs
            .insert(index.min(target_pane.tabs.len()), tab);
        target_pane.active = Some(tab);
        self.focus_pane(target);
        true
    }
    pub fn remove_tab(&mut self, tab: Id) {
        self.take_tab(tab, true);
    }
    pub fn split_tab(&mut self, tab: Id, axis: Axis) -> Option<Id> {
        let (window, pane, _) = self.take_tab(tab, false)?;
        let split = self.alloc();
        let new_pane = self.alloc();
        let source = self.window_mut(window)?.root.node_mut(pane)?;
        *source = Node::Split {
            id: split,
            axis,
            ratio: 0.5,
            first: Box::new(source.clone()),
            second: Box::new(Node::Pane(Pane {
                id: new_pane,
                tabs: vec![tab],
                active: Some(tab),
            })),
        };
        self.focus_pane(new_pane);
        Some(new_pane)
    }
    /// Drop a tab beside a target pane. `before` selects left/top; otherwise
    /// right/bottom. Connections and tab state remain owned by the caller.
    pub fn split_tab_into(&mut self, tab: Id, target: Id, axis: Axis, before: bool) -> Option<Id> {
        let (_, source) = self.tab_location(tab)?;
        let target_pane = self.pane(target)?;
        if source == target && target_pane.tabs.len() == 1 {
            return None;
        }
        if target_pane.tabs.is_empty() {
            return self.move_tab(tab, target, usize::MAX).then_some(target);
        }
        self.take_tab(tab, source != target)?;
        let window = self.pane_window(target)?;
        let split = self.alloc();
        let pane = self.alloc();
        let target_node = self.window_mut(window)?.root.node_mut(target)?;
        let original = Box::new(target_node.clone());
        let moved = Box::new(Node::Pane(Pane {
            id: pane,
            tabs: vec![tab],
            active: Some(tab),
        }));
        let (first, second) = if before {
            (moved, original)
        } else {
            (original, moved)
        };
        *target_node = Node::Split {
            id: split,
            axis,
            ratio: 0.5,
            first,
            second,
        };
        self.focus_pane(pane);
        Some(pane)
    }

    pub fn new_window(&mut self) -> Id {
        let id = self.alloc();
        let pane = self.alloc();
        self.windows.push(Window::empty(id, pane));
        self.active_window = id;
        id
    }
    pub fn detach_tab(&mut self, tab: Id) -> Option<Id> {
        self.tab_location(tab)?;
        let window = self.new_window();
        self.move_tab(tab, self.focused_pane(window)?, 0);
        Some(window)
    }
    fn take_pane(&mut self, pane: Id) -> Option<Pane> {
        let window = self.pane_window(pane)?;
        let contents = self.pane(pane)?.clone();
        let replacement = self.alloc();
        let source = self.window_mut(window)?;
        if !source.root.remove_pane(pane) {
            source.root = Node::Pane(Pane {
                id: replacement,
                tabs: Vec::new(),
                active: None,
            });
        }
        source.repair_focus();
        Some(contents)
    }
    pub fn detach_pane(&mut self, pane: Id) -> Option<Id> {
        let contents = self.take_pane(pane)?;
        let window = self.new_window();
        let target = self.window_mut(window)?;
        target.focused_pane = contents.id;
        target.root = Node::Pane(contents);
        Some(window)
    }
    /// Merge a group into another pane, preserving its order and active tab.
    pub fn merge_pane(&mut self, source: Id, target: Id) -> bool {
        if source == target || self.pane(target).is_none() {
            return false;
        }
        let Some(contents) = self.take_pane(source) else {
            return false;
        };
        let target_pane = self.pane_mut(target).unwrap();
        target_pane.tabs.extend(contents.tabs);
        if contents.active.is_some() {
            target_pane.active = contents.active;
        }
        self.focus_pane(target);
        true
    }

    /// Move a whole tab group without merging it into another group's tab order.
    pub fn move_pane_to_window(&mut self, pane: Id, target: Id) -> bool {
        if self.window(target).is_none() || self.pane_window(pane).is_none_or(|id| id == target) {
            return false;
        }
        let contents = self.take_pane(pane).unwrap();
        let split = self.alloc();
        let target = self.window_mut(target).unwrap();
        if matches!(&target.root, Node::Pane(pane) if pane.tabs.is_empty()) {
            target.root = Node::Pane(contents);
        } else {
            target.root = Node::Split {
                id: split,
                axis: Axis::Horizontal,
                ratio: 0.5,
                first: Box::new(target.root.clone()),
                second: Box::new(Node::Pane(contents)),
            };
        }
        self.focus_pane(pane);
        true
    }
    pub fn remove_window(&mut self, id: Id) -> bool {
        if id == 0
            || self
                .window(id)
                .is_none_or(|window| window.root.panes().iter().any(|p| !p.tabs.is_empty()))
        {
            return false;
        }
        self.windows.retain(|w| w.id != id);
        if self.active_window == id {
            self.active_window = 0;
        }
        true
    }
    pub fn set_ratio(&mut self, id: Id, ratio: f32) -> bool {
        self.windows.iter_mut().any(|w| w.root.set_ratio(id, ratio))
    }
    pub fn remap_tabs(&mut self, map: impl Fn(Id) -> Option<Id>) {
        for window in &mut self.windows {
            window.root.remap_tabs(&map);
        }
    }
    pub fn from_legacy(tabs: &[Id], selected: usize, split: bool, split_tab: usize) -> Self {
        let mut result = Self::default();
        for tab in tabs {
            result.add_tab(0, *tab);
        }
        let active = tabs
            .get(selected.min(tabs.len().saturating_sub(1)))
            .copied();
        if split && tabs.len() >= 2 {
            let mut index = split_tab.min(tabs.len() - 1);
            if Some(tabs[index]) == active {
                index = (index + 1) % tabs.len();
            }
            result.split_tab(tabs[index], Axis::Horizontal);
        }
        if let Some(tab) = active {
            result.focus_tab(tab);
        }
        result
    }
    /// Repair stale/malformed persisted references while preserving deliberate empty splits.
    pub fn normalize(&mut self, valid_tabs: &[Id]) {
        let mut used = HashSet::from([0]);
        let mut next = 1;
        // Repair all layout IDs, including u64::MAX, without trusting persisted next_id.
        fn claim(id: &mut Id, used: &mut HashSet<Id>, next: &mut Id) {
            if *id == Id::MAX || used.contains(id) {
                while used.contains(next) {
                    *next += 1;
                }
                *id = *next;
            }
            used.insert(*id);
            while used.contains(next) {
                *next += 1;
            }
        }
        fn repair(
            node: &mut Node,
            used: &mut HashSet<Id>,
            next: &mut Id,
            valid: &HashSet<Id>,
            tabs: &mut HashSet<Id>,
            focus: &mut Id,
        ) {
            match node {
                Node::Pane(pane) => {
                    let old = pane.id;
                    claim(&mut pane.id, used, next);
                    if *focus == old {
                        *focus = pane.id;
                    }
                    pane.tabs
                        .retain(|id| valid.contains(id) && tabs.insert(*id));
                    if pane.active.is_none_or(|id| !pane.tabs.contains(&id)) {
                        pane.active = pane.tabs.first().copied();
                    }
                }
                Node::Split {
                    id,
                    ratio,
                    first,
                    second,
                    ..
                } => {
                    claim(id, used, next);
                    *ratio = finite_clamp(*ratio, 0.1, 0.9, 0.5);
                    repair(first, used, next, valid, tabs, focus);
                    repair(second, used, next, valid, tabs, focus);
                }
            }
        }
        let valid: HashSet<_> = valid_tabs.iter().copied().collect();
        let mut tabs = HashSet::new();
        if !self.windows.iter().any(|w| w.id == 0) {
            self.windows.insert(0, Window::empty(0, 1));
        }
        let mut root_seen = false;
        for window in &mut self.windows {
            let old = window.id;
            if window.id == 0 && !root_seen {
                root_seen = true;
            } else {
                claim(&mut window.id, &mut used, &mut next);
            }
            if self.active_window == old {
                self.active_window = window.id;
            }
            repair(
                &mut window.root,
                &mut used,
                &mut next,
                &valid,
                &mut tabs,
                &mut window.focused_pane,
            );
            window.repair_focus();
            window.size = [
                finite_clamp(window.size[0], 520.0, 16384.0, 1000.0),
                finite_clamp(window.size[1], 400.0, 16384.0, 720.0),
            ];
            if window
                .position
                .is_some_and(|p| p.iter().any(|v| !v.is_finite()))
            {
                window.position = None;
            }
        }
        // Allocations must not collide with any large but valid restored ID.
        self.next_id = used.iter().max().copied().unwrap_or(0).saturating_add(1);
        if self.next_id == Id::MAX {
            // Extremely large IDs are harmless to retain; find a free low range instead.
            self.next_id = next;
        }
        if self.window(self.active_window).is_none() {
            self.active_window = 0;
        }
        let active_window = self.active_window;
        let active = self.active_tab(0);
        for tab in valid_tabs {
            if tabs.insert(*tab) {
                self.add_tab(0, *tab);
            }
        }
        if let Some(tab) = active {
            self.focus_tab(tab);
        }
        self.active_window = active_window;
    }
}

fn finite_clamp(value: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Workspace {
        Workspace::from_legacy(&[10, 20, 30, 40], 0, false, 0)
    }

    #[test]
    fn edge_split_preserves_tabs_across_panes_and_windows() {
        for axis in [Axis::Horizontal, Axis::Vertical] {
            for before in [false, true] {
                let mut ws = fixture();
                let target = ws.focused_pane(0).unwrap();
                let source_window = ws.detach_tab(20).unwrap();
                let pane = ws.split_tab_into(20, target, axis, before).unwrap();
                assert_eq!(ws.tab_location(20), Some((0, pane)));
                assert_eq!(ws.active_tab(0), Some(20));
                assert!(
                    ws.window(source_window).unwrap().root.panes()[0]
                        .tabs
                        .is_empty()
                );
                let Node::Split {
                    axis: actual,
                    first,
                    second,
                    ..
                } = &ws.window(0).unwrap().root
                else {
                    panic!("expected a split");
                };
                assert_eq!(*actual, axis);
                assert_eq!(if before { first } else { second }.panes()[0].id, pane);
                assert_tabs(&ws, &[10, 20, 30, 40]);
            }
        }
    }

    #[test]
    fn edge_split_collapses_empty_source_and_targets_nested_pane() {
        let mut ws = fixture();
        let source = ws.split_tab(20, Axis::Horizontal).unwrap();
        let target = ws.split_tab(30, Axis::Vertical).unwrap();
        let moved = ws
            .split_tab_into(20, target, Axis::Horizontal, true)
            .unwrap();
        assert!(ws.pane(source).is_none());
        assert_eq!(ws.pane(target).unwrap().tabs, [30]);
        assert_eq!(ws.pane(moved).unwrap().tabs, [20]);
        assert_eq!(ws.window(0).unwrap().root.panes().len(), 3);
        assert_tabs(&ws, &[10, 20, 30, 40]);
    }

    #[test]
    fn edge_split_rejects_stale_and_single_tab_self_drops() {
        let mut ws = Workspace::from_legacy(&[10], 0, false, 0);
        let pane = ws.focused_pane(0).unwrap();
        let original = ws.clone();
        assert!(
            ws.split_tab_into(10, pane, Axis::Horizontal, false)
                .is_none()
        );
        assert!(ws.split_tab_into(10, 999, Axis::Vertical, false).is_none());
        assert!(
            ws.split_tab_into(999, pane, Axis::Vertical, false)
                .is_none()
        );
        assert_eq!(ws, original);
        let window = ws.new_window();
        let empty = ws.focused_pane(window).unwrap();
        assert_eq!(
            ws.split_tab_into(10, empty, Axis::Horizontal, false),
            Some(empty)
        );
        assert_eq!(ws.window(window).unwrap().root.panes().len(), 1);
        assert_tabs(&ws, &[10]);
    }
    fn assert_tabs(ws: &Workspace, expected: &[Id]) {
        let mut tabs: Vec<_> = ws
            .windows
            .iter()
            .flat_map(|w| w.root.panes())
            .flat_map(|p| p.tabs.clone())
            .collect();
        tabs.sort_unstable();
        let mut expected = expected.to_vec();
        expected.sort_unstable();
        assert_eq!(tabs, expected);
        for w in &ws.windows {
            assert!(w.root.panes().iter().any(|p| p.id == w.focused_pane));
            for p in w.root.panes() {
                assert!(
                    p.active.is_none() && p.tabs.is_empty()
                        || p.active.is_some_and(|id| p.tabs.contains(&id))
                );
            }
        }
    }
    #[test]
    fn nested_splits_reorder_move_and_collapse() {
        let mut ws = fixture();
        let right = ws.split_tab(20, Axis::Horizontal).unwrap();
        let down = ws.split_tab(30, Axis::Vertical).unwrap();
        assert_eq!(ws.window(0).unwrap().root.panes().len(), 3);
        ws.move_tab(40, right, 0);
        assert_eq!(ws.pane(right).unwrap().tabs, [40, 20]);
        ws.move_tab(40, right, 2);
        assert_eq!(ws.pane(right).unwrap().tabs, [20, 40]);
        ws.move_tab(30, right, 1);
        assert!(ws.pane(down).is_none());
        assert_tabs(&ws, &[10, 20, 30, 40]);
    }
    #[test]
    fn detach_group_and_return_preserves_order_and_selection() {
        let mut ws = fixture();
        let pane = ws.focused_pane(0).unwrap();
        ws.focus_tab(20);
        let window = ws.detach_pane(pane).unwrap();
        assert_eq!(ws.active_tab(window), Some(20));
        assert!(ws.move_pane_to_window(pane, 0));
        assert!(ws.remove_window(window));
        assert_eq!(ws.pane(pane).unwrap().tabs, [10, 20, 30, 40]);
        assert_eq!(ws.active_tab(0), Some(20));
        assert_tabs(&ws, &[10, 20, 30, 40]);
    }
    #[test]
    fn empty_split_survives_normalization_and_stale_actions_are_noops() {
        let mut ws = Workspace::from_legacy(&[10], 0, false, 0);
        ws.split_tab(10, Axis::Vertical);
        ws.normalize(&[10]);
        assert_eq!(ws.window(0).unwrap().root.panes().len(), 2);
        let before = ws.clone();
        assert!(!ws.move_tab(10, 999, 0));
        assert!(ws.detach_tab(999).is_none());
        assert!(!ws.remove_window(0));
        assert_eq!(before, ws);
    }
    #[test]
    fn normalize_deduplicates_repairs_and_restores_missing_tabs() {
        let mut ws = fixture();
        let pane = ws.focused_pane(0).unwrap();
        ws.pane_mut(pane).unwrap().tabs = vec![10, 10, 999];
        ws.pane_mut(pane).unwrap().active = Some(999);
        ws.windows[0].focused_pane = 999;
        ws.windows[0].size = [f32::NAN, -1.0];
        ws.windows.push(ws.windows[0].clone());
        ws.normalize(&[10, 20, 30]);
        assert_tabs(&ws, &[10, 20, 30]);
        assert_ne!(ws.windows[0].id, ws.windows[1].id);
        assert_ne!(ws.windows[0].focused_pane, ws.windows[1].focused_pane);
        assert_eq!(ws.windows[0].size, [1000.0, 400.0]);
    }
    #[test]
    fn sparse_extreme_ids_cannot_collide_with_new_allocations() {
        let mut ws = fixture();
        ws.windows[0].root = Node::Split {
            id: Id::MAX - 1,
            axis: Axis::Horizontal,
            ratio: 0.5,
            first: Box::new(Node::Pane(Pane {
                id: 2,
                tabs: vec![10],
                active: Some(10),
            })),
            second: Box::new(Node::Pane(Pane {
                id: 4,
                tabs: vec![20],
                active: Some(20),
            })),
        };
        ws.normalize(&[10, 20]);
        let a = ws.new_window();
        let b = ws.new_window();
        assert_ne!(a, b);
        let mut used = HashSet::new();
        for window in &ws.windows {
            assert!(used.insert(window.id));
            for pane in window.root.panes() {
                assert!(used.insert(pane.id));
            }
        }
        assert_tabs(&ws, &[10, 20]);
    }

    #[test]
    fn merging_group_preserves_order_selection_and_collapses_source() {
        let mut ws = fixture();
        let source = ws.focused_pane(0).unwrap();
        let target = ws.split_tab(40, Axis::Vertical).unwrap();
        ws.focus_tab(20);
        assert!(ws.merge_pane(source, target));
        assert_eq!(ws.pane(target).unwrap().tabs, [40, 10, 20, 30]);
        assert_eq!(ws.active_tab(0), Some(20));
        assert_eq!(ws.window(0).unwrap().root.panes().len(), 1);
        assert_tabs(&ws, &[10, 20, 30, 40]);
    }

    #[test]
    fn panel_layout_sync_preserves_local_state_and_removes_stale_entries() {
        let mut layout = PanelLayout {
            panels: vec![
                PanelEntry {
                    id: "local".into(),
                    slot: PanelSlot::Right,
                    order: 0,
                    hidden: true,
                    size: Some(321.0),
                },
                PanelEntry {
                    id: "stale".into(),
                    slot: PanelSlot::Left,
                    order: 0,
                    hidden: true,
                    size: Some(99.0),
                },
            ],
        };
        layout.sync([
            ("local".into(), PanelSlot::Bottom, 7),
            ("new".into(), PanelSlot::Left, 2),
        ]);
        assert_eq!(layout.panels.len(), 2);
        assert_eq!(layout.entry("local").unwrap().slot, PanelSlot::Right);
        assert!(layout.entry("local").unwrap().hidden);
        assert_eq!(layout.entry("local").unwrap().size, Some(321.0));
        assert_eq!(layout.entry("new").unwrap().slot, PanelSlot::Left);
        assert!(!layout.entry("new").unwrap().hidden);
        assert!(layout.entry("stale").is_none());
    }

    #[test]
    fn panel_layout_reorder_stays_within_slot_and_round_trips() {
        let mut layout = PanelLayout {
            panels: vec![
                PanelEntry {
                    id: "a".into(),
                    slot: PanelSlot::Bottom,
                    order: 0,
                    hidden: false,
                    size: None,
                },
                PanelEntry {
                    id: "b".into(),
                    slot: PanelSlot::Bottom,
                    order: 1,
                    hidden: false,
                    size: Some(200.0),
                },
                PanelEntry {
                    id: "c".into(),
                    slot: PanelSlot::Left,
                    order: 0,
                    hidden: false,
                    size: None,
                },
            ],
        };
        layout.reorder("b", -1);
        assert_eq!(
            layout
                .panels
                .iter()
                .filter(|entry| entry.slot == PanelSlot::Bottom)
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["b", "a"]
        );
        assert_eq!(layout.panels[2].id, "c");
        let encoded = serde_json::to_string(&layout).unwrap();
        assert_eq!(
            serde_json::from_str::<PanelLayout>(&encoded).unwrap(),
            layout
        );
    }

    #[test]
    fn legacy_migration_keeps_other_tabs_left() {
        let ws = Workspace::from_legacy(&[10, 20, 30], 2, true, 1);
        assert_eq!(ws.window(0).unwrap().root.panes()[0].tabs, [10, 30]);
        assert_eq!(ws.window(0).unwrap().root.panes()[1].tabs, [20]);
        assert_eq!(ws.active_tab(0), Some(30));
    }
}
