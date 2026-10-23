use std::io::Read;
use std::time::{Duration, Instant};

use super::*;

fn sh() -> &'static Path {
    Path::new("/bin/sh")
}

/// Poll `cond` every 20 ms for up to `secs`.
fn eventually(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    cond()
}

#[test]
fn spawn_detached_runs_in_its_own_session_with_stderr_in_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("logs").join("worker.log");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, "earlier\n").unwrap();
    let pid = spawn_detached(
        sh(),
        &["-c", "echo to-stdout; echo started >&2; pwd >&2; sleep 5"],
        dir.path(),
        Some(&log),
    )
    .unwrap();
    assert!(
        eventually(5, || std::fs::read_to_string(&log).unwrap().lines().count()
            >= 3),
        "the child wrote its stderr into the log"
    );
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.starts_with("earlier\nstarted\n"), "appended: {text:?}");
    assert!(!text.contains("to-stdout"), "stdout goes nowhere: {text:?}");
    let cwd = std::fs::canonicalize(dir.path()).unwrap();
    assert!(text.contains(cwd.to_str().unwrap()), "cwd: {text:?}");

    // SAFETY: getsid only reads the process's session id.
    let sid = unsafe { libc::getsid(pid as i32) };
    assert_eq!(sid, pid as i32, "the child leads a new session");

    assert!(alive(pid));
    assert!(terminate(pid));
    assert!(
        eventually(5, || !alive(pid)),
        "a terminated detached child is reaped, not left a zombie"
    );
}

#[test]
fn spawn_supervised_pipes_both_streams() {
    let dir = tempfile::tempdir().unwrap();
    let mut child =
        spawn_supervised(sh(), &["-c", "echo out; echo err >&2; exit 3"], dir.path()).unwrap();
    let (mut out, mut err) = (String::new(), String::new());
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    assert_eq!(out, "out\n");
    assert_eq!(err, "err\n");
    assert_eq!(child.wait().unwrap().code(), Some(3));
}

#[test]
fn spawning_a_missing_program_fails() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-program");
    assert!(spawn_detached(&missing, &[], dir.path(), None).is_err());
    assert!(spawn_supervised(&missing, &[], dir.path()).is_err());
}

#[test]
fn pid_zero_and_out_of_range_pids_are_never_signalled() {
    assert!(!signal(0, 0), "pid 0 would address our own process group");
    assert!(!alive(0));
    assert!(
        !signal(u32::MAX, 0),
        "would be -1: every process we may signal"
    );
    assert!(!alive(u32::MAX));
}

#[test]
fn our_own_pid_is_alive() {
    assert!(alive(std::process::id()));
}
