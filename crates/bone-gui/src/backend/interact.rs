//! The pointer, as the nodes hear it: what is under it, pressed, clicked,
//! double-clicked or dragged. A frame's nodes say where they take the
//! pointer as they are painted ([`Pointer::sense`]); what is on top is
//! decided by the frame before's order, as the nodes are only known as
//! they are painted.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::geom::{Pos, Rect, Vec2};
use super::input::{Button, Event, FrameInput, Modifiers};

/// Moving this far with a button down is a drag, not a click.
const DRAG_DISTANCE: f32 = 6.0;
/// Two clicks this close in time are a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(300);
/// A press held longer than this is not a click.
const MAX_CLICK: Duration = Duration::from_millis(800);

/// What a node takes from the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sense {
    Hover,
    Click,
    /// Clicks and drags.
    Drag,
}

impl Sense {
    fn clicks(self) -> bool {
        self != Sense::Hover
    }
}

/// A node that takes the pointer, as it was painted.
#[derive(Clone, Debug)]
struct Hit {
    key: String,
    rect: Rect,
    sense: Sense,
}

/// A button's press or release this frame.
#[derive(Clone, Copy, Debug)]
pub struct Press {
    pub button: Button,
}

/// How a node fares with the pointer this frame.
#[derive(Clone, Debug, Default)]
pub struct Response {
    /// The pointer is over it, and nothing that takes it is above.
    pub hovered: bool,
    /// The pointer is over its rectangle, whatever is above.
    pub contains_pointer: bool,
    /// The primary button went down on it and is still down (or came up
    /// this frame).
    pub pressed_on: bool,
    pub clicked: bool,
    pub double_clicked: bool,
    pub secondary_clicked: bool,
    pub middle_clicked: bool,
    /// The primary button went down on it this frame.
    pub press_started: bool,
    /// It is being dragged (pressed on, and moved past a click's reach).
    pub dragged: bool,
    /// The drag ended this frame.
    pub drag_released: bool,
    /// Where the pointer is while it interacts with it.
    pub pointer: Option<Pos>,
}

#[derive(Default)]
pub struct Pointer {
    /// Where the pointer is, if over the window.
    pub pos: Option<Pos>,
    /// Its motion this frame.
    pub delta: Vec2,
    pub mods: Modifiers,
    down: HashMap<Button, (Pos, Instant)>,
    /// The node each held button went down on.
    captured: HashMap<Button, String>,
    /// This frame's presses, releases and clicks.
    pub pressed: Vec<Press>,
    pub released: Vec<Press>,
    clicks: Vec<(Button, String, Pos)>,
    double: Option<String>,
    last_click: Option<(Instant, Pos, String)>,
    /// Past a click's reach since the primary press.
    dragging: bool,
    /// Whether this frame's drag ended.
    drag_ended: bool,
    /// The nodes that take the pointer, last frame and this.
    prev: Vec<Hit>,
    hits: Vec<Hit>,
    /// The topmost under the pointer, from last frame's order.
    top: Option<String>,
}

impl Pointer {
    /// Take a frame's pointer events.
    pub fn begin(&mut self, input: &FrameInput) {
        self.prev = std::mem::take(&mut self.hits);
        self.pressed.clear();
        self.released.clear();
        self.clicks.clear();
        self.double = None;
        self.drag_ended = false;
        self.delta = Vec2::ZERO;
        self.mods = input.mods;
        // Captures last until the frame after the release, so the release
        // frame still reports the node.
        self.captured.retain(|b, _| self.down.contains_key(b));
        for ev in &input.events {
            match ev {
                Event::PointerMoved(p) => {
                    if let Some(old) = self.pos {
                        self.delta = self.delta + (*p - old);
                    }
                    self.pos = Some(*p);
                    if let Some((origin, _)) = self.down.get(&Button::Primary)
                        && origin.distance(*p) > DRAG_DISTANCE
                    {
                        self.dragging = true;
                    }
                }
                Event::PointerGone => self.pos = None,
                Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    mods,
                } => {
                    self.pos = Some(*pos);
                    self.mods = *mods;
                    let press = Press { button: *button };
                    if *pressed {
                        self.down.insert(*button, (*pos, input.time));
                        if *button == Button::Primary {
                            self.dragging = false;
                        }
                        if let Some(top) = self.top_at(*pos, true) {
                            self.captured.insert(*button, top);
                        } else {
                            self.captured.remove(button);
                        }
                        self.pressed.push(press);
                    } else {
                        let held = self.down.remove(button);
                        self.released.push(press);
                        if *button == Button::Primary && self.dragging {
                            self.drag_ended = true;
                        }
                        let quick = held.is_some_and(|(_, at)| input.time.duration_since(at) < MAX_CLICK);
                        let still = !(*button == Button::Primary && self.dragging);
                        if let (true, true, Some(key)) =
                            (quick, still, self.captured.get(button).cloned())
                            && self.top_at(*pos, true).as_deref() == Some(key.as_str())
                        {
                            if *button == Button::Primary {
                                if let Some((at, p, k)) = &self.last_click
                                    && *k == key
                                    && input.time.duration_since(*at) < DOUBLE_CLICK
                                    && p.distance(*pos) < DRAG_DISTANCE
                                {
                                    self.double = Some(key.clone());
                                    self.last_click = None;
                                } else {
                                    self.last_click = Some((input.time, *pos, key.clone()));
                                }
                            }
                            self.clicks.push((*button, key, *pos));
                        }
                    }
                }
                _ => {}
            }
        }
        self.top = self.pos.and_then(|p| self.top_at(p, false));
    }

    /// The topmost node of last frame under `p` (one that clicks, with
    /// `clicks`).
    fn top_at(&self, p: Pos, clicks: bool) -> Option<String> {
        (self.prev.iter().rev())
            .find(|h| h.rect.contains(p) && (!clicks || h.sense.clicks()))
            .map(|h| h.key.clone())
    }

    /// Whether `button` is held.
    pub fn is_down(&self, button: Button) -> bool {
        self.down.contains_key(&button)
    }

    pub fn primary_pressed(&self) -> bool {
        self.pressed.iter().any(|p| p.button == Button::Primary)
    }

    pub fn primary_released(&self) -> bool {
        self.released.iter().any(|p| p.button == Button::Primary)
    }

    /// Node `key` takes the pointer in `rect` (above what came before it
    /// this frame): how it fares.
    pub fn sense(&mut self, key: &str, rect: Rect, sense: Sense) -> Response {
        self.hits.push(Hit {
            key: key.to_owned(),
            rect,
            sense,
        });
        let contains = self.pos.is_some_and(|p| rect.contains(p));
        let captured = |b: Button| self.captured.get(&b).is_some_and(|k| k == key);
        let mine = captured(Button::Primary);
        // While a button is held on something, nothing else is hovered.
        let busy = (self.captured.get(&Button::Primary)).is_some_and(|k| k != key)
            && self.is_down(Button::Primary);
        let hovered = contains
            && !busy
            && match &self.top {
                Some(top) => top == key,
                // New this frame: nothing knew of it yet.
                None => !self.prev.iter().any(|h| h.key == key),
            };
        let clicked = |b: Button| {
            sense.clicks() && (self.clicks.iter()).any(|(button, k, _)| *button == b && k == key)
        };
        let pressed_on = sense.clicks()
            && mine
            && (self.is_down(Button::Primary) || self.primary_released());
        let dragging = sense == Sense::Drag && mine && self.dragging;
        Response {
            hovered: hovered || (pressed_on && self.is_down(Button::Primary)),
            contains_pointer: contains,
            pressed_on,
            press_started: sense.clicks()
                && mine
                && self.primary_pressed(),
            clicked: clicked(Button::Primary),
            double_clicked: sense.clicks() && self.double.as_deref() == Some(key),
            secondary_clicked: clicked(Button::Secondary),
            middle_clicked: clicked(Button::Middle),
            dragged: dragging && self.is_down(Button::Primary),
            drag_released: sense == Sense::Drag && mine && self.drag_ended,
            pointer: if mine || hovered { self.pos } else { None },
        }
    }
}
