use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct WorkTimer {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug)]
struct Inner {
    started: Instant,
    paused: Duration,
    pause_started: Option<Instant>,
}

impl WorkTimer {
    pub fn start() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                started: Instant::now(),
                paused: Duration::ZERO,
                pause_started: None,
            })),
        }
    }

    pub fn pause(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.pause_started.is_none() {
            inner.pause_started = Some(Instant::now());
        }
    }

    pub fn resume(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(started) = inner.pause_started.take() {
            inner.paused += started.elapsed();
        }
    }

    pub fn elapsed_ms(&self) -> u64 {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut paused = inner.paused;
        if let Some(started) = inner.pause_started {
            paused += started.elapsed();
        }
        inner.started.elapsed().saturating_sub(paused).as_millis() as u64
    }
}
