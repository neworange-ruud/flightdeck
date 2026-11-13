//! The TCP ports a session's processes listen on (`specs/SESSION_BROWSER_PRD.md`
//! B11): what the session browser's address field offers, and — for a worker
//! session — the loopback ports the Desktop forwards before a page asks
//! (B22, B24).
//!
//! **A port belongs to the session whose worktree its process runs in.** A
//! dev server an agent starts has the worktree (or a folder in it) as its
//! working directory, including one that detached from its shell (`nohup`,
//! `&`), which a walk down the PTY's process tree would miss. So a
//! [`ListenProbe`] lists every listening socket with its process's working
//! directory, and [`ports_under`] picks a worktree's.
//!
//! The probe runs a system tool, which can take a moment, so [`PortWatch`]
//! runs it on a thread of its own every [`SCAN_EVERY`] and only while someone
//! wants the answer ([`PortWatch::want`], renewed by whoever shows ports; it
//! lapses after [`LEASE`]). Nothing runs for a host nobody browses.
//!
//! - **macOS**: `lsof -nP -iTCP -sTCP:LISTEN` for the sockets, `lsof -d cwd`
//!   for the directories.
//! - **Linux**: `ss -ltnpH` for the sockets and their processes,
//!   `/proc/<pid>/cwd` for the directories.
//! - **Windows**: none yet (an empty list): the session browser does not run
//!   there.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How often the watch looks while wanted.
pub const SCAN_EVERY: Duration = Duration::from_secs(4);

/// How long one [`PortWatch::want`] keeps the watch looking.
pub const LEASE: Duration = Duration::from_secs(30);

/// One listening socket and where its process runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listener {
    pub port: u16,
    /// The owning process's working directory, when it could be read.
    pub cwd: Option<PathBuf>,
}

/// Lists this machine's listening TCP sockets. A seam so the watch is
/// tested without running system tools.
pub trait ListenProbe: Send + Sync {
    fn listening(&self) -> Vec<Listener>;
}

/// The listeners' ports whose process runs in `dir` or below it, sorted and
/// without repeats. `dir` is matched as given and canonicalised (macOS's
/// `/var` is `/private/var`).
pub fn ports_under(listeners: &[Listener], dir: &Path) -> Vec<u16> {
    if dir.as_os_str().is_empty() {
        return Vec::new();
    }
    let canonical = std::fs::canonicalize(dir).ok();
    let mut ports = BTreeSet::new();
    for listener in listeners {
        let Some(cwd) = &listener.cwd else { continue };
        let inside = cwd.starts_with(dir) || canonical.as_ref().is_some_and(|c| cwd.starts_with(c));
        if inside {
            ports.insert(listener.port);
        }
    }
    ports.into_iter().collect()
}

/// Each of `dirs`' ports (in order): a listener goes to the **deepest** dir
/// its process runs in, so a base-branch session (the repository root, which
/// contains every worktree) does not also claim its worktrees' servers.
pub fn assign(listeners: &[Listener], dirs: &[PathBuf]) -> Vec<Vec<u16>> {
    let canon: Vec<Option<PathBuf>> = dirs.iter().map(|d| std::fs::canonicalize(d).ok()).collect();
    let mut out: Vec<BTreeSet<u16>> = vec![BTreeSet::new(); dirs.len()];
    for listener in listeners {
        let Some(cwd) = &listener.cwd else { continue };
        let owner = dirs
            .iter()
            .enumerate()
            .filter(|(i, d)| {
                !d.as_os_str().is_empty()
                    && (cwd.starts_with(d)
                        || canon[*i].as_ref().is_some_and(|c| cwd.starts_with(c)))
            })
            .max_by_key(|(_, d)| d.components().count())
            .map(|(i, _)| i);
        if let Some(i) = owner {
            out[i].insert(listener.port);
        }
    }
    out.into_iter()
        .map(|ports| ports.into_iter().collect())
        .collect()
}

/// The port of a socket address as `lsof` and `ss` print it: `*:3000`,
/// `127.0.0.1:5173`, `[::1]:8000`, `0.0.0.0:8080`.
fn port_of(address: &str) -> Option<u16> {
    address.rsplit_once(':')?.1.parse().ok().filter(|p| *p != 0)
}

/// `lsof -nP -iTCP -sTCP:LISTEN -Fpn`: `p<pid>` lines, then each socket's
/// `n<address>` line. Returns `(pid, port)`s.
pub fn parse_lsof_listen(out: &str) -> Vec<(u32, u16)> {
    let mut pid = None;
    let mut found = Vec::new();
    for line in out.lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.trim().parse().ok();
        } else if let Some(name) = line.strip_prefix('n') {
            if let (Some(pid), Some(port)) = (pid, port_of(name.trim())) {
                found.push((pid, port));
            }
        }
    }
    found.sort_unstable();
    found.dedup();
    found
}

/// `lsof -a -d cwd -Fpn -p <pids>`: `p<pid>`, `fcwd`, `n<dir>` per process.
pub fn parse_lsof_cwd(out: &str) -> HashMap<u32, PathBuf> {
    let mut pid = None;
    let mut cwds = HashMap::new();
    for line in out.lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.trim().parse().ok();
        } else if let Some(dir) = line.strip_prefix('n') {
            if let Some(pid) = pid {
                cwds.insert(pid, PathBuf::from(dir.trim()));
            }
        }
    }
    cwds
}

/// `ss -ltnpH`: one listening socket a line, its local address in the fourth
/// column and its processes as `users:(("node",pid=1234,fd=20),…)`.
pub fn parse_ss(out: &str) -> Vec<(u32, u16)> {
    let mut found = Vec::new();
    for line in out.lines() {
        let columns: Vec<&str> = line.split_whitespace().collect();
        let Some(port) = columns.get(3).and_then(|a| port_of(a)) else {
            continue;
        };
        let mut rest = line;
        while let Some(at) = rest.find("pid=") {
            rest = &rest[at + 4..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if let Ok(pid) = digits.parse() {
                found.push((pid, port));
            }
        }
    }
    found.sort_unstable();
    found.dedup();
    found
}

/// The real probe: this platform's tools, as the module docs describe.
pub struct SystemProbe;

impl ListenProbe for SystemProbe {
    fn listening(&self) -> Vec<Listener> {
        #[cfg(target_os = "macos")]
        {
            let Some(listen) = run("lsof", &["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpn"]) else {
                return Vec::new();
            };
            let sockets = parse_lsof_listen(&listen);
            let pids: BTreeSet<String> = sockets.iter().map(|(pid, _)| pid.to_string()).collect();
            if pids.is_empty() {
                return Vec::new();
            }
            let list = pids.into_iter().collect::<Vec<_>>().join(",");
            let cwds = run("lsof", &["-a", "-d", "cwd", "-Fpn", "-p", &list])
                .map(|out| parse_lsof_cwd(&out))
                .unwrap_or_default();
            sockets
                .into_iter()
                .map(|(pid, port)| Listener {
                    port,
                    cwd: cwds.get(&pid).cloned(),
                })
                .collect()
        }
        #[cfg(target_os = "linux")]
        {
            let Some(out) = run("ss", &["-ltnpH"]) else {
                return Vec::new();
            };
            parse_ss(&out)
                .into_iter()
                .map(|(pid, port)| Listener {
                    port,
                    cwd: std::fs::read_link(format!("/proc/{pid}/cwd")).ok(),
                })
                .collect()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Vec::new()
        }
    }
}

/// Run `program` with `args` and return its stdout, when it ran.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    // lsof exits 1 when it found nothing, which is an answer too.
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// See the module docs.
pub struct PortWatch {
    probe: Arc<dyn ListenProbe>,
    latest: Arc<Mutex<Vec<Listener>>>,
    /// Milliseconds since `epoch` at which the lease ends.
    wanted_until: Arc<AtomicU64>,
    epoch: Instant,
    /// Whether the thread is running.
    running: Arc<std::sync::atomic::AtomicBool>,
}

impl PortWatch {
    /// A watch over `probe`, not looking until wanted.
    pub fn new(probe: Arc<dyn ListenProbe>) -> PortWatch {
        PortWatch {
            probe,
            latest: Arc::default(),
            wanted_until: Arc::new(AtomicU64::new(0)),
            epoch: Instant::now(),
            running: Arc::default(),
        }
    }

    /// Keep looking for [`LEASE`] from now, starting the thread if it is not
    /// running.
    pub fn want(&self) {
        let until = self.epoch.elapsed() + LEASE;
        self.wanted_until
            .store(until.as_millis() as u64, Ordering::Relaxed);
        if self.running.swap(true, Ordering::AcqRel) {
            return;
        }
        let probe = self.probe.clone();
        let latest = self.latest.clone();
        let wanted_until = self.wanted_until.clone();
        let running = self.running.clone();
        let epoch = self.epoch;
        let spawned = std::thread::Builder::new()
            .name("flightdeck-ports".to_string())
            .spawn(move || loop {
                if epoch.elapsed().as_millis() as u64 > wanted_until.load(Ordering::Relaxed) {
                    // Lapsed: stop, and forget, so a stale list is never shown.
                    latest.lock().unwrap_or_else(|e| e.into_inner()).clear();
                    running.store(false, Ordering::Release);
                    return;
                }
                let found = probe.listening();
                *latest.lock().unwrap_or_else(|e| e.into_inner()) = found;
                std::thread::sleep(SCAN_EVERY);
            });
        if spawned.is_err() {
            self.running.store(false, Ordering::Release);
        }
    }

    /// The ports listened on in `dir` or below it, as last seen.
    pub fn ports_under(&self, dir: &Path) -> Vec<u16> {
        let latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        ports_under(&latest, dir)
    }

    /// Each of `dirs`' ports, as last seen ([`assign`]).
    pub fn assign(&self, dirs: &[PathBuf]) -> Vec<Vec<u16>> {
        let latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        assign(&latest, dirs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listener(port: u16, cwd: &str) -> Listener {
        Listener {
            port,
            cwd: Some(PathBuf::from(cwd)),
        }
    }

    #[test]
    fn a_port_belongs_to_the_worktree_its_process_runs_in() {
        let listeners = vec![
            listener(3000, "/repo/.flightdeck/worktrees/login"),
            listener(5173, "/repo/.flightdeck/worktrees/login/web"),
            listener(8000, "/repo/.flightdeck/worktrees/search"),
            listener(7420, "/home/me"),
            Listener {
                port: 9000,
                cwd: None,
            },
            listener(3000, "/repo/.flightdeck/worktrees/login"),
        ];
        assert_eq!(
            ports_under(&listeners, Path::new("/repo/.flightdeck/worktrees/login")),
            vec![3000, 5173]
        );
        assert_eq!(
            ports_under(&listeners, Path::new("/repo/.flightdeck/worktrees/search")),
            vec![8000]
        );
        // A prefix that is not a folder boundary is not inside.
        assert!(ports_under(&listeners, Path::new("/repo/.flightdeck/worktrees/log")).is_empty());
        assert!(ports_under(&listeners, Path::new("")).is_empty());
    }

    #[test]
    fn a_port_goes_to_the_deepest_worktree_its_process_runs_in() {
        let listeners = vec![
            listener(3000, "/repo/.flightdeck/worktrees/login"),
            listener(4000, "/repo"),
            listener(4001, "/repo/src"),
        ];
        let dirs = [
            PathBuf::from("/repo"),
            PathBuf::from("/repo/.flightdeck/worktrees/login"),
            PathBuf::from(""),
        ];
        assert_eq!(
            assign(&listeners, &dirs),
            vec![vec![4000, 4001], vec![3000], Vec::<u16>::new()]
        );
    }

    #[test]
    fn lsof_output_gives_each_process_its_ports_and_folder() {
        let listen = "p101\nf20\nn*:3000\nf21\nn[::1]:3000\np202\nf5\nn127.0.0.1:5173\n";
        assert_eq!(parse_lsof_listen(listen), vec![(101, 3000), (202, 5173)]);
        let cwd = "p101\nfcwd\nn/repo/wt/a\np202\nfcwd\nn/repo/wt/b web\n";
        let cwds = parse_lsof_cwd(cwd);
        assert_eq!(cwds[&101], PathBuf::from("/repo/wt/a"));
        assert_eq!(cwds[&202], PathBuf::from("/repo/wt/b web"));
    }

    #[test]
    fn ss_output_gives_each_socket_its_processes() {
        let out = "LISTEN 0      511          0.0.0.0:3000      0.0.0.0:*    users:((\"node\",pid=1234,fd=21))\n\
                   LISTEN 0      4096            [::]:8080         [::]:*    users:((\"python3\",pid=55,fd=3),(\"python3\",pid=56,fd=3))\n\
                   LISTEN 0      128        127.0.0.1:631      0.0.0.0:*\n";
        assert_eq!(parse_ss(out), vec![(55, 8080), (56, 8080), (1234, 3000)]);
    }

    struct Fixed(Vec<Listener>);

    impl ListenProbe for Fixed {
        fn listening(&self) -> Vec<Listener> {
            self.0.clone()
        }
    }

    #[test]
    fn the_watch_answers_once_wanted() {
        let watch = PortWatch::new(Arc::new(Fixed(vec![listener(3000, "/wt/a")])));
        assert!(
            watch.ports_under(Path::new("/wt/a")).is_empty(),
            "nothing before it is wanted"
        );
        watch.want();
        let mut seen = Vec::new();
        for _ in 0..100 {
            seen = watch.ports_under(Path::new("/wt/a"));
            if !seen.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(seen, vec![3000]);
    }

    /// The real probe, against a real server: a port is found under the
    /// folder its process runs in. macOS (`lsof`) and Linux (`ss`).
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_system_probe_finds_a_server_by_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = free.local_addr().unwrap().port();
        drop(free);
        let Ok(mut server) = std::process::Command::new("python3")
            .args([
                "-m",
                "http.server",
                &port.to_string(),
                "--bind",
                "127.0.0.1",
            ])
            .current_dir(dir.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            eprintln!("no python3: skipped");
            return;
        };
        let mut found = Vec::new();
        for _ in 0..50 {
            found = ports_under(&SystemProbe.listening(), dir.path());
            if found.contains(&port) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = server.kill();
        let _ = server.wait();
        assert!(found.contains(&port), "{port} not in {found:?}");
    }
}
