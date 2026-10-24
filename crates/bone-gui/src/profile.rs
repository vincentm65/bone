//! Frame timings for finding GUI bottlenecks. Off unless
//! [`enable`] is called, and then a relaxed flag check. Not a public
//! feature: the perf test turns it on.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static ON: AtomicBool = AtomicBool::new(false);

thread_local! {
    static ACC: RefCell<Vec<(&'static str, Duration, u32)>> = const { RefCell::new(Vec::new()) };
}

pub fn enable(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

pub struct Span {
    name: &'static str,
    t: Option<Instant>,
}

impl Span {
    pub fn start(name: &'static str) -> Self {
        Self {
            name,
            t: enabled().then(Instant::now),
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some(t) = self.t else {
            return;
        };
        let dt = t.elapsed();
        ACC.with(|a| {
            let mut a = a.borrow_mut();
            if let Some(e) = a.iter_mut().find(|e| e.0 == self.name) {
                e.1 += dt;
                e.2 += 1;
            } else {
                a.push((self.name, dt, 1));
            }
        });
    }
}

pub fn reset() {
    ACC.with(|a| a.borrow_mut().clear());
}

pub fn report() -> String {
    ACC.with(|a| {
        let a = a.borrow();
        let mut parts: Vec<_> = a
            .iter()
            .map(|(n, d, c)| {
                format!(
                    "{n}={:.2}ms/{} ({:.3}ms)",
                    d.as_secs_f64() * 1000.0,
                    c,
                    d.as_secs_f64() * 1000.0 / f64::from(*c)
                )
            })
            .collect();
        parts.sort();
        parts.join(" ")
    })
}
