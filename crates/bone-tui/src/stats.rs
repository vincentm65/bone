//! `BONE_FRAME_STATS=<file>`: append one line per frame with how long each
//! stage took (µs), and the time spent handling events since the last frame.

use std::cell::RefCell;
use std::fs::File;
use std::io::Write;
use std::sync::OnceLock;
use std::time::Instant;

fn enabled() -> bool {
    static PATH: OnceLock<Option<std::ffi::OsString>> = OnceLock::new();
    PATH.get_or_init(|| std::env::var_os("BONE_FRAME_STATS"))
        .is_some()
}

#[derive(Default)]
struct State {
    file: Option<File>,
    frame: Option<(Instant, Instant, String)>,
    events_us: u128,
    events: u64,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// Time `f` as event handling.
pub fn event<R>(f: impl FnOnce() -> R) -> R {
    if !enabled() {
        return f();
    }
    let start = Instant::now();
    let r = f();
    STATE.with_borrow_mut(|s| {
        s.events_us += start.elapsed().as_micros();
        s.events += 1;
    });
    r
}

pub fn begin() {
    if enabled() {
        let now = Instant::now();
        STATE.with_borrow_mut(|s| s.frame = Some((now, now, String::new())));
    }
}

/// The time since the last mark (or `begin`) went to `stage`.
pub fn mark(stage: &str) {
    if !enabled() {
        return;
    }
    STATE.with_borrow_mut(|s| {
        if let Some((_, last, line)) = &mut s.frame {
            let now = Instant::now();
            line.push_str(&format!(" {stage}={}", (now - *last).as_micros()));
            *last = now;
        }
    });
}

pub fn finish() {
    if !enabled() {
        return;
    }
    STATE.with_borrow_mut(|s| {
        let Some((start, _, line)) = s.frame.take() else {
            return;
        };
        if s.file.is_none() {
            let path = std::env::var_os("BONE_FRAME_STATS").unwrap_or_default();
            s.file = File::options().create(true).append(true).open(path).ok();
        }
        let total = start.elapsed().as_micros();
        let (us, n) = (
            std::mem::take(&mut s.events_us),
            std::mem::take(&mut s.events),
        );
        if let Some(f) = &mut s.file {
            let _ = writeln!(f, "total={total}{line} events={us}/{n}");
        }
    });
}
