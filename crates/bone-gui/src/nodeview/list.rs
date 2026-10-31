//! Virtual lists: measure the items in view, and keep a chat item's node
//! until the shared render cache says it changed.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use bone_ui::lua::Host;
use mlua::Value;

use crate::Gui;
use crate::flex::{self, Placed, Rect};
use crate::node::Sink;
use crate::node::{self, Kind, List, Node, TextStyle};

use super::{Fonts, Scroll, Target};

/// A list's measured heights and the items made for the window in view.
#[derive(Default)]
pub(crate) struct ListState {
    key: String,
    heights: Vec<f32>,
    /// Which heights are measured, not the list's estimate.
    measured: Vec<bool>,
    gap: f32,
    estimate: f32,
    width: f32,
    pub(super) nodes: HashMap<usize, Rc<Node>>,
    /// A chat list's items made last frame, by chat item: kept while the
    /// chat says they are fresh.
    chat: HashMap<bone_ui::chat::Item, Rc<Node>>,
    /// The running item made last, and when: see [`STREAM_EVERY`].
    streamed: Option<(bone_ui::chat::Item, Instant)>,
}

impl ListState {
    /// Forget made nodes (their callbacks belong to a Lua state that is
    /// going away) but keep measured heights, so a reload does not snap.
    pub(crate) fn drop_nodes(&mut self) {
        self.nodes.clear();
        self.chat.clear();
    }

    /// `n` items came before the first: they take their estimate until
    /// measured, and what was in view keeps its place (the offset moves).
    pub(crate) fn prepend(&mut self, n: usize, scroll: Option<&mut Scroll>) {
        let est = self.estimate.max(1.0);
        self.heights.splice(0..0, std::iter::repeat_n(est, n));
        self.measured.splice(0..0, std::iter::repeat_n(false, n));
        // Made nodes go by index.
        self.nodes.clear();
        if let Some(sc) = scroll {
            let moved = n as f32 * (est + self.gap);
            sc.offset += moved;
            sc.max += moved;
        }
    }
}

/// How Lua moves the chat list (`bone.chat.scroll`, `scroll_to`).
pub(crate) enum ChatMove {
    /// Rows of text down (negative: up).
    By(f32),
    Top,
    Bottom,
    /// To this many pixels from the start.
    To(f32),
}

/// A running item (an answer being streamed) is made again at most this
/// often: each delta would otherwise remake all of it, which grows with its
/// length. The view wakes when the time is up.
const STREAM_EVERY: Duration = Duration::from_millis(50);

/// A chat list's items this frame (see `Gui::chat_items`). Built only when
/// a visible item is missing from the cache.
struct ChatItems {
    /// The chat buffer the list shows.
    buf: usize,
    items: Rc<[bone_ui::chat::Item]>,
    kept: HashMap<bone_ui::chat::Item, Rc<Node>>,
    used: HashMap<bone_ui::chat::Item, Rc<Node>>,
    data: u64,
    width: usize,
    generation: (u64, u64),
    now: Instant,
    streamed: Option<(bone_ui::chat::Item, Instant)>,
}

/// The ids of the functions node `n` and what is in it carry.
fn handler_ids(n: &Node, out: &mut Vec<u64>) {
    let h = &n.handlers;
    out.extend(
        [
            h.click,
            h.secondary,
            h.middle,
            h.double_click,
            h.press,
            h.release,
            h.drag,
            h.hover,
            h.scroll,
            h.key,
        ]
        .into_iter()
        .flatten(),
    );
    match &n.kind {
        Kind::Canvas(c) => out.push(c.draw),
        Kind::TextInput(t) => out.extend(t.on_change.into_iter().chain(t.on_submit)),
        _ => {}
    }
    for c in &n.children {
        handler_ids(c, out);
    }
}

/// The items of one list that are in view, made and placed.
pub(super) struct Window {
    pub(super) nodes: Vec<Rc<Node>>,
    pub(super) first: usize,
    pub(super) rects: Vec<Rect>,
}

/// Lists' windows by id, and what paint draws from them.
pub(super) type Windows = HashMap<String, Window>;

/// Each list's total height, from what has been measured of it.
pub(super) fn set_totals(n: &mut Node, lists: &mut HashMap<String, ListState>) {
    if let (Kind::List(l), Some(id)) = (&mut n.kind, &n.id) {
        let gaps = l.gap * l.count.saturating_sub(1) as f32;
        l.total = match lists.get(id).filter(|st| st.key == l.key) {
            Some(st) => {
                let known: f32 = st.heights.iter().take(l.count).sum();
                let unknown = l.count.saturating_sub(st.heights.len()) as f32 * l.estimate;
                known + unknown + gaps
            }
            None => l.count as f32 * l.estimate + gaps,
        };
    }
    for c in &mut n.children {
        set_totals(c, lists);
    }
}

impl Gui {
    /// Make and measure the items of each list that are in view, move the
    /// list's offset for what that changed, and give each its window.
    /// True when heights changed enough to want another frame.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn realize(
        &mut self,
        p: &Placed,
        fonts: &Fonts,
        base: &TextStyle,
        sink: &Sink,
        rebuilt: bool,
        lists: &mut HashMap<String, ListState>,
        scroll: &mut HashMap<String, Scroll>,
        out: &mut Windows,
    ) -> bool {
        let mut again = false;
        if let (Kind::List(l), Some(id)) = (&p.node.kind, &p.node.id) {
            again |= self.realize_list(p, l, id, fonts, base, sink, rebuilt, lists, scroll, out);
        }
        for c in &p.children {
            again |= self.realize(c, fonts, base, sink, rebuilt, lists, scroll, out);
        }
        again
    }

    #[allow(clippy::too_many_arguments)]
    fn realize_list(
        &mut self,
        p: &Placed,
        l: &List,
        id: &str,
        fonts: &Fonts,
        base: &TextStyle,
        sink: &Sink,
        rebuilt: bool,
        lists: &mut HashMap<String, ListState>,
        scroll: &mut HashMap<String, Scroll>,
        out: &mut Windows,
    ) -> bool {
        let est = l.estimate.max(1.0);
        let st = lists.entry(id.to_owned()).or_default();
        if st.key != l.key {
            *st = ListState {
                key: l.key.clone(),
                ..Default::default()
            };
        }
        st.heights.resize(l.count, est);
        st.measured.resize(l.count, false);
        (st.gap, st.estimate) = (l.gap, est);
        let width = p.inner.w;
        if (st.width - width).abs() > 0.5 {
            // Another width wraps differently: what was seen is a guess.
            st.width = width;
            st.nodes.clear();
        }
        let sc = scroll.entry(id.to_owned()).or_default();
        let old_max = sc.max;
        let at_end = !sc.held && !sc.dragging && sc.offset >= old_max - 1.0;
        let viewport = p.inner.h;
        let (mut offset, gap) = (sc.offset, l.gap);

        // The first item to make: a couple above the one at the top.
        let (mut first, mut top) = (0, 0.0);
        while first < l.count && top + st.heights[first] + gap <= offset {
            top += st.heights[first] + gap;
            first += 1;
        }
        let seen = first;
        for _ in 0..2 {
            if first == 0 {
                break;
            }
            first -= 1;
            top -= st.heights[first] + gap;
        }
        let first_top = top;
        // What was on screen stays where it was: an item measured for the
        // first time above the first one that was (scrolling up into the
        // unknown) moves the offset, not it. With none of those in reach
        // (a jump), the item at the top is held.
        let reach = offset + viewport + viewport / 2.0;
        let pin = {
            let (mut j, mut y) = (first, first_top);
            while j < l.count && y < reach && !st.measured[j] {
                y += st.heights[j] + gap;
                j += 1;
            }
            if j < l.count && y < reach {
                j.max(seen)
            } else {
                seen
            }
        };

        let buf = l.buffer.unwrap_or(self.current);
        let is_chat = l.chat && buf < self.chats.len();
        let mut nodes = Vec::new();
        let mut cached = std::mem::take(&mut st.nodes);
        // The whole transcript is read only when a visible item is missing.
        let mut chat = None;
        let mut y = top;
        let mut again = false;
        let mut i = first;
        while i < l.count && (y < offset + viewport + viewport / 2.0 || i < seen + 2) {
            let _item_lua = crate::profile::Span::start("item-lua");
            let prev = cached.remove(&i);
            let item = match prev.clone() {
                Some(n) if !rebuilt => n,
                _ if is_chat => {
                    if chat.is_none() {
                        let kept = std::mem::take(&mut st.chat);
                        chat = Some(self.chat_items(buf, width, kept, st.streamed.take()));
                    }
                    self.chat_item(l, i, base, sink, chat.as_mut().unwrap())
                }
                _ => self.make_item(l, i, base, sink),
            };
            drop(_item_lua);
            if let Some(chat) = chat.as_mut()
                && let Some(key) = chat.items.get(i).copied()
            {
                chat.used.insert(key, Rc::clone(&item));
            }
            let mg = item.style.margin;
            // The same node at the same width (a change of either gives another
            // node or clears `cached`) is as tall as it was.
            let height = if st.measured[i] && prev.is_some_and(|p| Rc::ptr_eq(&p, &item)) {
                st.heights[i]
            } else {
                let _item_measure = crate::profile::Span::start("item-measure");
                let z = flex::natural_at(&item, (width - mg.horizontal()).max(0.0), fonts);
                z.h + mg.vertical()
            };
            let delta = height - st.heights[i];
            if delta.abs() > 0.5 {
                again = true;
                if i < pin {
                    offset += delta;
                }
            }
            st.heights[i] = height;
            st.measured[i] = true;
            y += height + gap;
            nodes.push(item);
            i += 1;
        }
        if let Some(chat) = chat {
            st.chat = chat.used;
            st.streamed = chat.streamed;
            // An item that asked to be drawn again (`bone.chat.refresh_in`)
            // wakes the view then; one out of view waits to come back.
            let now = Instant::now();
            let next = (self.chats.get(buf)).and_then(|c| c.next_expiry_after(now));
            self.lua.chat_expiry = self.lua.chat_expiry.into_iter().chain(next).min();
        }
        let total = st.heights.iter().sum::<f32>() + gap * l.count.saturating_sub(1) as f32;
        let max = (total - viewport).max(0.0);
        let follow = p.node.style.follow;
        sc.offset = if follow && at_end { max } else { offset }.clamp(0.0, max);
        (sc.max, sc.viewport) = (max, viewport);
        if is_chat && buf == self.current && sc.offset < viewport {
            self.load_earlier();
        }

        // Where each item sits on screen.
        let mut rects = Vec::with_capacity(nodes.len());
        let mut y = first_top;
        for (k, n) in nodes.iter().enumerate() {
            let mg = n.style.margin;
            let height = st.heights[first + k];
            rects.push(Rect {
                x: p.inner.x + mg.left,
                y: p.inner.y + y - sc.offset + mg.top,
                w: (width - mg.horizontal()).max(0.0),
                h: (height - mg.vertical()).max(0.0),
            });
            y += height + gap;
        }
        out.insert(
            id.to_owned(),
            Window {
                nodes,
                first,
                rects,
            },
        );
        again
    }

    /// What a chat list needs to keep its items: the chat's items, which of
    /// them have changed (the shared render cache's rules: their data, the
    /// views, options and colors, what they read of other items, a
    /// `refresh_in`), and the nodes made last frame.
    fn chat_items(
        &mut self,
        buf: usize,
        width: f32,
        kept: HashMap<bone_ui::chat::Item, Rc<Node>>,
        streamed: Option<(bone_ui::chat::Item, Instant)>,
    ) -> ChatItems {
        let generation = Host::generation(self);
        let data = bone_ui::lua::check_chat_deps(self, buf);
        let now = Instant::now();
        let items = self.chats[buf].items();
        ChatItems {
            buf,
            items,
            kept,
            used: HashMap::new(),
            data,
            width: width.round() as usize,
            generation,
            now,
            streamed,
        }
    }

    /// Chat item `i` (0-based) of list `l`: the node made before while the
    /// chat says it has not changed, else made again, with what its view
    /// reads recorded. Freshness is one item, not the whole transcript.
    fn chat_item(
        &mut self,
        l: &List,
        i: usize,
        base: &TextStyle,
        sink: &Sink,
        chat: &mut ChatItems,
    ) -> Rc<Node> {
        let buf = chat.buf;
        let Some(&item) = chat.items.get(i) else {
            return self.chat_make_item(buf, l, i, base, sink);
        };
        let key = self.chats[buf].stale_key(&chat.items, i, chat.width, chat.generation, chat.now);
        // Changed, but only just made, and still running: keep it a moment.
        let wait = chat.streamed.and_then(|(it, at)| {
            let due = at + STREAM_EVERY;
            (it == item && key.is_some() && chat.now < due).then_some(due)
        });
        if let Some(due) = wait
            && self.chats[buf].item_status(item).1
            && let Some(node) = chat.kept.remove(&item)
        {
            let wake = &mut self.view.wake;
            *wake = Some(wake.map_or(due, |w| w.min(due)));
            return node;
        }
        if key.is_none()
            && let Some(node) = chat.kept.remove(&item)
        {
            // After a rebuild its functions are still in the table of the
            // tree before: they come along.
            if let Some(old) = &self.view.bound.prev_sink {
                let mut ids = Vec::new();
                handler_ids(&node, &mut ids);
                for id in ids {
                    if matches!(sink.table.raw_get::<Value>(id), Ok(Value::Nil)) {
                        let _ = sink
                            .table
                            .raw_set(id, old.raw_get::<Value>(id).unwrap_or(Value::Nil));
                    }
                }
            }
            return node;
        }
        self.lua.render_deps = Some(Vec::new());
        self.lua.render_expires = None;
        let node = self.chat_make_item(buf, l, i, base, sink);
        if self.chats[buf].item_status(item).1 {
            chat.streamed = Some((item, chat.now));
        }
        let deps = self.lua.render_deps.take().unwrap_or_default();
        let expires = self.lua.render_expires.take();
        if let Some(key) = key {
            self.chats[buf].store(item, key, Vec::new(), deps, expires, chat.data);
        }
        node
    }

    /// Item `i` of a list of chat `buf`: chat queries without a session
    /// read that chat while its view runs.
    fn chat_make_item(
        &mut self,
        buf: usize,
        l: &List,
        i: usize,
        base: &TextStyle,
        sink: &Sink,
    ) -> Rc<Node> {
        let was = self.lua.rendering.replace(buf);
        let node = self.make_item(l, i, base, sink);
        self.lua.rendering = was;
        node
    }

    /// Item `i` (0-based) of `l`, from Lua; an error shows as an empty node.
    fn make_item(&mut self, l: &List, i: usize, base: &TextStyle, sink: &Sink) -> Rc<Node> {
        let item = l.item.clone();
        let r = self.with_api(|_| item.call::<mlua::Table>(i + 1));
        let parsed = match r {
            Ok(t) => node::parse(&t, &*self, base, sink).map_err(mlua::Error::runtime),
            Err(e) => Err(e),
        };
        Rc::new(parsed.unwrap_or_else(|e| {
            Host::lua_error(self, "gui list item", &e);
            Node::empty()
        }))
    }

    /// Move the chat list, at the next frame.
    pub(crate) fn move_chat(&mut self, to: ChatMove) {
        let line_px = self.number_option("scroll_line", 20.0).max(1.0);
        let s = self.view.scroll.entry("chat".into()).or_default();
        s.target = Some(match to {
            ChatMove::By(rows) => Target::To((s.offset + rows * line_px).clamp(0.0, s.max)),
            ChatMove::Top => Target::To(0.0),
            ChatMove::Bottom => Target::End,
            ChatMove::To(px) => Target::To(px),
        });
        self.view.invalidate();
    }

    /// Where item `index` (from 1) of the chat list is: its top and height
    /// in the list's pixels, and the list's viewport.
    pub(crate) fn item_span(&self, index: usize) -> Option<(f32, f32, f32)> {
        self.list_item_span("chat", index)
    }

    /// The same for the list with id `id`.
    pub(crate) fn list_item_span(&self, id: &str, index: usize) -> Option<(f32, f32, f32)> {
        let st = self.view.lists.get(id)?;
        let viewport = self.view.scroll.get(id)?.viewport;
        let i = index.checked_sub(1).filter(|i| *i < st.heights.len())?;
        let top: f32 = st.heights[..i].iter().sum::<f32>() + st.gap * i as f32;
        Some((top, st.heights[i], viewport))
    }

    /// The chat list's scroll position and the items in view.
    pub(crate) fn chat_view(&self) -> serde_json::Value {
        let (Some(st), Some(sc)) = (self.view.lists.get("chat"), self.view.scroll.get("chat"))
        else {
            return serde_json::json!({ "top": 0, "height": 0, "rows": 0, "follow": true });
        };
        let (mut y, mut first, mut last) = (0.0, None, None);
        for (i, h) in st.heights.iter().enumerate() {
            if y + h > sc.offset && y < sc.offset + sc.viewport {
                first.get_or_insert(i + 1);
                last = Some(i + 1);
            }
            y += h + st.gap;
        }
        serde_json::json!({
            "top": sc.offset,
            "height": sc.viewport,
            "rows": sc.viewport + sc.max,
            "follow": sc.offset >= sc.max - 1.0,
            "first": first,
            "last": last,
        })
    }
}
