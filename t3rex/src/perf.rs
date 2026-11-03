//! Opt-in presentation counters, without conversation contents.

use std::time::{Duration, Instant};

pub struct Frames {
    enabled: bool,
    since: Instant,
    frames: u64,
    bytes: usize,
    cpu: Duration,
}

impl Frames {
    pub fn new() -> Self {
        Self {
            enabled: std::env::var_os("T3REX_PERF").is_some(),
            since: Instant::now(),
            frames: 0,
            bytes: 0,
            cpu: Duration::ZERO,
        }
    }

    pub fn record(&mut self, build: Duration, bytes: usize) {
        if !self.enabled {
            return;
        }
        self.frames += 1;
        self.bytes += bytes;
        self.cpu += build;
        if self.since.elapsed() >= Duration::from_secs(5) {
            eprintln!(
                "t3rex perf: frames={} terminal_bytes={} build_us={} elapsed_ms={}",
                self.frames,
                self.bytes,
                self.cpu.as_micros(),
                self.since.elapsed().as_millis()
            );
            self.since = Instant::now();
            self.frames = 0;
            self.bytes = 0;
            self.cpu = Duration::ZERO;
        }
    }
}
