use super::*;
use crate::testing::FakePty;
use std::path::PathBuf;

const SIZE: PtySize = PtySize {
    rows: 30,
    cols: 100,
};

fn started(pty: &FakePty) -> WorkerShell {
    let mut shell = WorkerShell::new(SIZE);
    shell
        .start_with(pty, Path::new("/home/u"), Some("/bin/zsh"), &|_| true)
        .unwrap();
    shell
}

fn drained(shell: &mut WorkerShell) -> Vec<u8> {
    let mut out = Vec::new();
    shell.drain(&mut |b| out.extend_from_slice(b));
    out
}

#[test]
fn login_shell_prefers_an_absolute_existing_shell() {
    let all = |_: &Path| true;
    assert_eq!(
        login_shell(Some("/bin/zsh"), &all),
        ("/bin/zsh".to_string(), vec!["-l".to_string()])
    );
}

#[test]
fn login_shell_falls_back_to_bin_sh() {
    let fallback = ("/bin/sh".to_string(), vec!["-l".to_string()]);
    let all = |_: &Path| true;
    let none = |_: &Path| false;
    assert_eq!(login_shell(None, &all), fallback);
    assert_eq!(login_shell(Some(""), &all), fallback);
    assert_eq!(login_shell(Some("  "), &all), fallback);
    assert_eq!(login_shell(Some("zsh"), &all), fallback, "not absolute");
    assert_eq!(login_shell(Some("/bin/zsh"), &none), fallback, "missing");
}

#[test]
fn start_spawns_the_login_shell_in_home_with_a_terminal_env() {
    let pty = FakePty::new();
    let shell = started(&pty);
    assert_eq!(
        pty.spawns(),
        vec![(
            "/bin/zsh".to_string(),
            vec!["-l".to_string()],
            PathBuf::from("/home/u")
        )]
    );
    let env = &pty.spawn_envs()[0];
    assert!(env.contains(&("TERM".to_string(), "xterm-256color".to_string())));
    assert!(env.contains(&("COLORTERM".to_string(), "truecolor".to_string())));
    let facts = shell.facts().unwrap();
    assert_eq!(facts.title, "Worker shell");
    assert_eq!(
        facts.geometry,
        Geometry {
            cols: 100,
            rows: 30
        }
    );
    assert!(facts.alive);
    assert_eq!(facts.exit_code, None);
}

#[test]
fn facts_are_none_before_the_first_start() {
    let mut shell = WorkerShell::new(SIZE);
    assert_eq!(shell.facts(), None);
    assert_eq!(shell.poll_exit(), None);
    assert!(drained(&mut shell).is_empty());
}

#[test]
fn a_failed_spawn_is_an_error_and_leaves_no_shell() {
    let pty = FakePty::new();
    pty.fail_next_spawn();
    let mut shell = WorkerShell::new(SIZE);
    assert!(shell.start(&pty, Path::new("/home/u")).is_err());
    assert_eq!(shell.facts(), None);
}

#[test]
fn drain_tees_every_chunk_and_reports_whether_it_read() {
    let pty = FakePty::new();
    let h = pty.queue_session();
    let mut shell = started(&pty);
    assert!(!shell.drain(&mut |_| panic!("nothing to tee")));
    h.push_output(b"hello ".to_vec());
    h.push_output(b"world".to_vec());
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    assert!(shell.drain(&mut |b| chunks.push(b.to_vec())));
    assert_eq!(chunks, vec![b"hello ".to_vec(), b"world".to_vec()]);
    assert!(!shell.drain(&mut |_| panic!("drained")));
}

#[test]
fn drain_feeds_the_emulator_and_answers_a_cursor_query() {
    let pty = FakePty::new();
    let h = pty.queue_session();
    let mut shell = started(&pty);
    h.push_output(b"hi\x1b[6n".to_vec());
    drained(&mut shell);
    let reply = String::from_utf8(h.input()).unwrap();
    assert!(
        reply.starts_with("\x1b[") && reply.ends_with('R'),
        "{reply:?}"
    );
}

#[test]
fn write_reaches_a_running_shell() {
    let pty = FakePty::new();
    let h = pty.queue_session();
    let mut shell = started(&pty);
    assert_eq!(shell.write(b"ls\r"), Written::Ok);
    assert_eq!(h.input(), b"ls\r");
}

#[test]
fn write_before_start_or_after_exit_is_not_running() {
    let mut shell = WorkerShell::new(SIZE);
    assert_eq!(shell.write(b"x"), Written::NotRunning);
    let pty = FakePty::new();
    let h = pty.queue_session();
    let mut shell = started(&pty);
    h.set_state(ProcessState::Exited(0));
    assert_eq!(shell.write(b"x"), Written::NotRunning);
    assert!(h.input().is_empty());
}

#[test]
fn the_exit_edge_is_reported_once() {
    let pty = FakePty::new();
    let h = pty.queue_session();
    let mut shell = started(&pty);
    assert_eq!(shell.poll_exit(), None);
    h.set_state(ProcessState::Exited(3));
    assert_eq!(shell.poll_exit(), Some(Some(3)));
    assert_eq!(shell.poll_exit(), None);
    let facts = shell.facts().unwrap();
    assert!(!facts.alive);
    assert_eq!(facts.exit_code, Some(3));
}

#[test]
fn a_killed_shell_exits_without_a_code() {
    let pty = FakePty::new();
    let h = pty.queue_session();
    let mut shell = started(&pty);
    h.set_state(ProcessState::Stopped);
    assert_eq!(shell.poll_exit(), Some(None));
    assert_eq!(shell.poll_exit(), None);
}

#[test]
fn output_left_behind_by_an_exited_shell_still_drains() {
    let pty = FakePty::new();
    let h = pty.queue_session();
    let mut shell = started(&pty);
    h.push_output(b"logout\r\n".to_vec());
    h.set_state(ProcessState::Exited(0));
    assert_eq!(drained(&mut shell), b"logout\r\n");
}

#[test]
fn restart_after_exit_spawns_again_at_the_last_size() {
    let pty = FakePty::new();
    let first = pty.queue_session();
    let mut shell = started(&pty);
    assert!(shell.resize(PtySize {
        rows: 40,
        cols: 120
    }));
    first.set_state(ProcessState::Exited(0));
    assert_eq!(shell.poll_exit(), Some(Some(0)));

    let second = pty.queue_session();
    shell.restart(&pty, Path::new("/home/u")).unwrap();
    assert_eq!(pty.spawns().len(), 2);
    assert!(!first.terminated(), "an exited shell is not killed again");
    assert_eq!(
        shell.facts().unwrap().geometry,
        Geometry {
            cols: 120,
            rows: 40
        }
    );
    assert_eq!(shell.write(b"x"), Written::Ok);
    assert_eq!(second.input(), b"x");
    // The new shell's own exit is a new edge.
    second.set_state(ProcessState::Exited(1));
    assert_eq!(shell.poll_exit(), Some(Some(1)));
    assert_eq!(shell.starts, 2);
}

#[test]
fn restart_terminates_a_live_shell_first() {
    let pty = FakePty::new();
    let first = pty.queue_session();
    let mut shell = started(&pty);
    shell.restart(&pty, Path::new("/home/u")).unwrap();
    assert!(first.terminated());
    assert_eq!(pty.spawns().len(), 2);
}

#[test]
fn resize_forwards_and_equal_is_a_noop() {
    let pty = FakePty::new();
    let h = pty.queue_session();
    let mut shell = started(&pty);
    assert!(!shell.resize(SIZE), "same size");
    assert!(h.resizes().is_empty());
    assert!(shell.resize(PtySize {
        rows: 50,
        cols: 160
    }));
    assert_eq!(
        h.resizes(),
        vec![PtySize {
            rows: 50,
            cols: 160
        }]
    );
    assert!(!shell.resize(PtySize {
        rows: 50,
        cols: 160
    }));
    assert_eq!(h.resizes().len(), 1);
    assert_eq!(
        shell.facts().unwrap().geometry,
        Geometry {
            cols: 160,
            rows: 50
        }
    );
}

#[test]
fn resize_before_start_sets_the_spawn_size() {
    let mut shell = WorkerShell::new(SIZE);
    assert!(!shell.resize(SIZE));
    assert!(shell.resize(PtySize { rows: 10, cols: 50 }));
    let pty = FakePty::new();
    shell
        .start_with(&pty, Path::new("/home/u"), None, &|_| false)
        .unwrap();
    assert_eq!(pty.spawns()[0].0, "/bin/sh");
    assert_eq!(
        shell.facts().unwrap().geometry,
        Geometry { cols: 50, rows: 10 }
    );
}

#[test]
fn terminate_stops_a_live_shell_and_is_safe_twice() {
    let pty = FakePty::new();
    let h = pty.queue_session();
    let mut shell = started(&pty);
    shell.terminate();
    assert!(h.terminated());
    shell.terminate();
    assert_eq!(shell.write(b"x"), Written::NotRunning);
    WorkerShell::new(SIZE).terminate();
}
