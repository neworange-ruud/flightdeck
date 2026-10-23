//! A worker's health, as the Desktop's Workers list shows it
//! (`specs/WORKER_PRD.md` §4.5): load, memory, free disk, agent counts.
//!
//! The probe is a trait so the sampler is testable against
//! [`FakeHealthProbe`]; [`RealHealthProbe`] asks the OS through `libc` (unix)
//! and reports nothing elsewhere. No new dependency: `getloadavg`, `statvfs`,
//! `/proc/meminfo` (Linux) and `sysctlbyname("hw.memsize")` (macOS) answer
//! everything (design A10). Available memory is `None` on macOS in v1: the
//! kernel has no single honest number for it.

use std::path::Path;

use crate::web::protocol::Health;

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
        os::load_avg_1m()
    }

    fn cpu_count(&self) -> u32 {
        std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1)
    }

    fn memory(&self) -> (Option<u64>, Option<u64>) {
        os::memory()
    }

    /// The nearest existing ancestor answers for a path that does not exist
    /// yet (a `projects_root` nobody has cloned into): it is the filesystem a
    /// clone would land on.
    fn disk_free(&self, path: &Path) -> Option<u64> {
        let existing = path.ancestors().find(|p| p.exists())?;
        os::disk_free(existing)
    }
}

#[cfg(unix)]
mod os {
    use std::path::Path;

    /// Widen any unsigned OS integer to `u64`. A generic so the call compiles
    /// cleanly whether the platform's type already is `u64` or narrower.
    fn widen<T: Into<u64>>(v: T) -> u64 {
        v.into()
    }

    pub(super) fn load_avg_1m() -> Option<f32> {
        let mut loads = [0f64; 3];
        // SAFETY: `loads` has room for the 3 samples asked for.
        let n = unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) };
        (n >= 1).then(|| loads[0] as f32)
    }

    pub(super) fn disk_free(path: &Path) -> Option<u64> {
        use std::os::unix::ffi::OsStrExt;
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: `statvfs` is plain old data; zeroed is a valid value and the
        // call only writes into it.
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: `c_path` is NUL-terminated and `stat` is a valid out pointer.
        let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
        if rc != 0 {
            return None;
        }
        Some(widen(stat.f_bavail).saturating_mul(widen(stat.f_frsize)))
    }

    #[cfg(target_os = "linux")]
    pub(super) fn memory() -> (Option<u64>, Option<u64>) {
        match std::fs::read_to_string("/proc/meminfo") {
            Ok(text) => super::parse_meminfo(&text),
            Err(_) => (None, None),
        }
    }

    #[cfg(target_os = "macos")]
    pub(super) fn memory() -> (Option<u64>, Option<u64>) {
        let mut total: u64 = 0;
        let mut len = std::mem::size_of::<u64>();
        // SAFETY: the name is NUL-terminated; `total`/`len` describe a u64
        // buffer, which is what `hw.memsize` answers with.
        let rc = unsafe {
            libc::sysctlbyname(
                c"hw.memsize".as_ptr(),
                (&mut total as *mut u64).cast::<libc::c_void>(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        ((rc == 0 && total > 0).then_some(total), None)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(super) fn memory() -> (Option<u64>, Option<u64>) {
        (None, None)
    }
}

#[cfg(not(unix))]
mod os {
    use std::path::Path;

    pub(super) fn load_avg_1m() -> Option<f32> {
        None
    }

    pub(super) fn disk_free(_path: &Path) -> Option<u64> {
        None
    }

    pub(super) fn memory() -> (Option<u64>, Option<u64>) {
        (None, None)
    }
}

/// `MemTotal` and `MemAvailable` out of `/proc/meminfo`'s text, in bytes
/// (what [`RealHealthProbe::memory`] reads on Linux).
pub fn parse_meminfo(text: &str) -> (Option<u64>, Option<u64>) {
    let field = |name: &str| {
        text.lines().find_map(|line| {
            let rest = line.strip_prefix(name)?.strip_prefix(':')?;
            let mut parts = rest.split_whitespace();
            let value: u64 = parts.next()?.parse().ok()?;
            match parts.next() {
                Some("kB") => Some(value.saturating_mul(1024)),
                None => Some(value),
                Some(_) => None,
            }
        })
    };
    (field("MemTotal"), field("MemAvailable"))
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

/// What a sample counts besides the probe's answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AgentCounts {
    pub total: u32,
    pub running: u32,
}

/// Decides when the next sample is due, and takes it.
#[derive(Debug, Default)]
pub struct HealthSampler {
    /// The host clock when the worker started, for `uptime_secs`.
    started_ms: u64,
    /// The host clock at the last sample.
    last_ms: Option<u64>,
}

impl HealthSampler {
    /// A sampler for a worker that started at `started_ms` (host clock).
    pub fn new(started_ms: u64) -> Self {
        HealthSampler {
            started_ms,
            last_ms: None,
        }
    }

    /// Whether a sample is due at `now_ms` (every [`HEALTH_INTERVAL_MS`]; the
    /// first one at once).
    pub fn due(&self, now_ms: u64) -> bool {
        self.last_ms
            .is_none_or(|last| now_ms.saturating_sub(last) >= HEALTH_INTERVAL_MS)
    }

    /// Take a sample at `now_ms`, and remember that it was taken.
    pub fn sample(
        &mut self,
        probe: &dyn HealthProbe,
        now_ms: u64,
        home: &Path,
        projects_root: &Path,
        agents: AgentCounts,
    ) -> Health {
        self.last_ms = Some(now_ms);
        let (mem_total_bytes, mem_available_bytes) = probe.memory();
        Health {
            sampled_at_ms: now_ms as i64,
            uptime_secs: now_ms.saturating_sub(self.started_ms) / 1000,
            version: crate::VERSION.to_string(),
            load_avg_1m_centi: probe
                .load_avg_1m()
                .filter(|l| l.is_finite() && *l >= 0.0)
                .map(|l| (l * 100.0).round() as u32),
            cpu_count: probe.cpu_count(),
            mem_total_bytes,
            mem_available_bytes,
            disk_free_home_bytes: probe.disk_free(home),
            disk_free_projects_root_bytes: probe.disk_free(projects_root),
            agents_total: agents.total,
            agents_running: agents.running,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_sample_is_due_at_once_then_every_ten_seconds() {
        let mut sampler = HealthSampler::new(1_000);
        let probe = FakeHealthProbe {
            load_avg_1m: Some(1.234),
            cpu_count: 8,
            memory_total: Some(16),
            memory_available: Some(4),
            disk_free: Some(99),
        };
        assert!(sampler.due(1_000));
        let health = sampler.sample(
            &probe,
            21_000,
            Path::new("/home/me"),
            Path::new("/home/me/flightdeck"),
            AgentCounts {
                total: 3,
                running: 2,
            },
        );
        assert_eq!(health.uptime_secs, 20);
        assert_eq!(health.sampled_at_ms, 21_000);
        assert_eq!(health.load_avg_1m_centi, Some(123));
        assert_eq!(health.cpu_count, 8);
        assert_eq!(health.mem_total_bytes, Some(16));
        assert_eq!(health.mem_available_bytes, Some(4));
        assert_eq!(health.disk_free_home_bytes, Some(99));
        assert_eq!(health.disk_free_projects_root_bytes, Some(99));
        assert_eq!((health.agents_total, health.agents_running), (3, 2));
        assert_eq!(health.version, crate::VERSION);
        assert!(!sampler.due(30_999));
        assert!(sampler.due(31_000));
    }

    #[test]
    fn meminfo_is_read_in_bytes() {
        let text = "MemTotal:       16384 kB\nMemFree:  1 kB\nMemAvailable:    8192 kB\n";
        assert_eq!(parse_meminfo(text), (Some(16384 * 1024), Some(8192 * 1024)));
        assert_eq!(parse_meminfo("nothing"), (None, None));
    }

    #[cfg(unix)]
    #[test]
    fn the_real_probe_answers_on_unix() {
        let probe = RealHealthProbe;
        assert!(probe.cpu_count() >= 1);
        assert!(probe.load_avg_1m().is_some());
        let tmp = std::env::temp_dir();
        assert!(probe.disk_free(&tmp).is_some());
        // A path that does not exist yet answers for its nearest ancestor.
        assert!(probe.disk_free(&tmp.join("no/such/dir/yet")).is_some());
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(probe.memory().0.is_some());
    }
}
