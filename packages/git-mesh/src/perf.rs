//! Opt-in performance logging for CLI operation groups.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn init(cli_enabled: bool) {
    ENABLED.store(cli_enabled || env_enabled(), Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

fn env_enabled() -> bool {
    match std::env::var("GIT_MESH_PERF") {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

pub struct Span {
    label: &'static str,
    start: Option<Instant>,
}

impl Span {
    pub fn new(label: &'static str) -> Self {
        Self {
            label,
            start: enabled().then(Instant::now),
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some(start) = self.start else {
            return;
        };
        let elapsed = start.elapsed();
        eprintln!(
            "git-mesh perf: {} {:.3} ms",
            self.label,
            elapsed.as_secs_f64() * 1000.0
        );
    }
}

pub fn span(label: &'static str) -> Span {
    Span::new(label)
}

pub fn counter(label: &str, value: u64) {
    if !enabled() {
        return;
    }
    eprintln!("git-mesh perf: {label} {value}");
}
