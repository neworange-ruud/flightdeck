//! A worker's health, as the Desktop's Workers list shows it
//! (`specs/WORKER_PRD.md` §4.5): load, memory, free disk, agent counts.
//!
//! The probe is a trait so the sampler is testable against
//! [`FakeHealthProbe`]; [`RealHealthProbe`] asks the OS through `libc` (unix)
//! and reports nothing elsewhere.

// stub: filled by C (RealHealthProbe over libc, and `HealthSampler::sample(…)
// -> Health`, which needs protocol v7's `Health`: to be wired after P0a merges)

use std::path::Path;

/// How often a worker samples its health (milliseconds of host clock).
pub const HEALTH_INTERVAL_MS: u64 = 10_000;

/// Reads the machine's vital signs.
pub trait HealthProbe {
    /// The 1-minute load average.
    fn load_avg_1m(&self) -> Option<f32>;
    /// Logical CPUs.
    fn cpu_count(&self) -> u32;
    /// `(total, available)` memory in bytes.
    fn memory(&self) -> (Option<u64>, Option<u64>);
    /// Free bytes on the filesystem holding `path`, available to this user.
    fn disk_free(&self, path: &Path) -> Option<u64>;
}

/// The OS's answers.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealHealthProbe;

impl HealthProbe for RealHealthProbe {
    fn load_avg_1m(&self) -> Option<f32> {
        // stub: filled by C
        None
    }

    fn cpu_count(&self) -> u32 {
        // stub: filled by C
        1
    }

    fn memory(&self) -> (Option<u64>, Option<u64>) {
        // stub: filled by C
        (None, None)
    }

    fn disk_free(&self, _path: &Path) -> Option<u64> {
        // stub: filled by C
        None
    }
}

/// Fixed answers, for tests.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FakeHealthProbe {
    pub load_avg_1m: Option<f32>,
    pub cpu_count: u32,
    pub memory_total: Option<u64>,
    pub memory_available: Option<u64>,
    /// Answered for every path.
    pub disk_free: Option<u64>,
}

impl HealthProbe for FakeHealthProbe {
    fn load_avg_1m(&self) -> Option<f32> {
        self.load_avg_1m
    }

    fn cpu_count(&self) -> u32 {
        self.cpu_count
    }

    fn memory(&self) -> (Option<u64>, Option<u64>) {
        (self.memory_total, self.memory_available)
    }

    fn disk_free(&self, _path: &Path) -> Option<u64> {
        self.disk_free
    }
}

/// Decides when the next sample is due.
#[derive(Debug, Default)]
pub struct HealthSampler {
    // stub: filled by C (last sample time, start time)
    _stub: (),
}

impl HealthSampler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a sample is due at `now_ms` (every [`HEALTH_INTERVAL_MS`]).
    pub fn due(&self, _now_ms: u64) -> bool {
        // stub: filled by C
        false
    }
}
