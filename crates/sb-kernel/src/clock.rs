//! Injectable time source.

use std::sync::Mutex;

use chrono::{DateTime, Utc};

/// A source of the current time.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A manually controlled clock for tests.
#[derive(Debug)]
pub struct FixedClock(Mutex<DateTime<Utc>>);

impl FixedClock {
    pub fn new(t: DateTime<Utc>) -> Self {
        FixedClock(Mutex::new(t))
    }

    pub fn set(&self, t: DateTime<Utc>) {
        if let Ok(mut g) = self.0.lock() {
            *g = t;
        }
    }

    pub fn advance(&self, d: chrono::Duration) {
        if let Ok(mut g) = self.0.lock() {
            *g += d;
        }
    }
}

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        match self.0.lock() {
            Ok(g) => *g,
            Err(p) => *p.into_inner(),
        }
    }
}
