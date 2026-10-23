use super::*;

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

fn run(args: &[&str]) -> (i32, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = dispatch(&strings(args), &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn parse_table() {
    let logs = |follow, lines| Ok(Command::Logs { follow, lines });
    let follow_flag = format!("-{}", 'f');
    let cases: Vec<(Vec<&str>, Result<Command, ParseError>)> = vec![
        (vec![], Ok(Command::Help)),
        (vec!["--help"], Ok(Command::Help)),
        (vec!["help"], Ok(Command::Help)),
        (vec!["start", "--help"], Ok(Command::Help)),
        (vec!["start"], Ok(Command::Start)),
        (vec!["stop"], Ok(Command::Stop)),
        (vec!["restart"], Ok(Command::Restart)),
        (vec!["run"], Ok(Command::Run)),
        (vec!["supervise"], Ok(Command::Supervise)),
        (vec!["status"], Ok(Command::Status { json: false })),
        (vec!["status", "--json"], Ok(Command::Status { json: true })),
        (vec!["pair", "--json"], Ok(Command::Pair { json: true })),
        (vec!["devices"], Ok(Command::Devices { json: false })),
        (
            vec!["revoke", "mbp"],
            Ok(Command::Revoke {
                device: "mbp".to_string(),
            }),
        ),
        (vec!["logs"], logs(false, 100)),
        (vec!["logs", "-n", "5"], logs(false, 5)),
        (vec!["logs", "-n5"], logs(false, 5)),
        (vec!["logs", "--lines", "0"], logs(false, 0)),
        (vec!["logs", "--follow"], logs(true, 100)),
        (vec!["logs", &follow_flag], logs(true, 100)),
        (vec!["logs", &follow_flag, "-n", "20"], logs(true, 20)),
        (vec!["logs", "-n", "20", "--follow"], logs(true, 20)),
        (
            vec!["frobnicate"],
            Err(ParseError::Unknown("frobnicate".to_string())),
        ),
    ];
    for (args, expected) in cases {
        assert_eq!(parse(&strings(&args)), expected, "{args:?}");
    }
}

#[test]
fn misused_commands_are_usage_errors() {
    for args in [
        &["start", "now"][..],
        &["stop", "--json"],
        &["status", "--yaml"],
        &["status", "--json", "--json"],
        &["revoke"],
        &["revoke", "a", "b"],
        &["revoke", " "],
        &["logs", "-n"],
        &["logs", "-n", "many"],
        &["logs", "-n", "-3"],
        &["logs", "--tail"],
    ] {
        assert!(
            matches!(parse(&strings(args)), Err(ParseError::Usage(_))),
            "{args:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn no_command_or_help_prints_the_table() {
    for args in [&[][..], &["--help"], &["help"], &["logs", "-h"]] {
        let (code, out, _) = run(args);
        assert_eq!(code, 0, "{args:?}");
        assert_eq!(out, USAGE);
    }
    assert!(USAGE.contains("revoke <device>"));
    assert!(USAGE.contains("logs [-f] [-n N]"));
    assert!(
        !USAGE.contains("supervise"),
        "the internal command is not advertised"
    );
}

#[cfg(unix)]
#[test]
fn an_unknown_command_prints_the_table_and_fails() {
    let (code, out, err) = run(&["frobnicate"]);
    assert_eq!(code, 2);
    assert!(out.is_empty());
    assert!(err.contains("unknown command 'frobnicate'"), "{err}");
    assert!(err.contains(USAGE), "{err}");
}

#[cfg(unix)]
#[test]
fn a_usage_error_exits_2_with_a_pointer_to_help() {
    let (code, out, err) = run(&["logs", "-n", "many"]);
    assert_eq!(code, 2);
    assert!(out.is_empty());
    assert!(
        err.contains("takes a number of lines (got 'many')"),
        "{err}"
    );
    assert!(err.contains("flightdeck worker --help"), "{err}");
}

#[cfg(not(unix))]
#[test]
fn every_command_is_refused_off_unix() {
    for args in [&[][..], &["start"], &["--help"]] {
        let (code, _, err) = run(args);
        assert_eq!(code, 2);
        assert_eq!(err.trim_end(), NOT_SUPPORTED);
    }
}

#[cfg(unix)]
mod lifecycle {
    use super::*;

    fn home() -> (tempfile::TempDir, WorkerPaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = WorkerPaths::from_home(dir.path());
        (dir, paths)
    }

    fn call(f: impl FnOnce(&mut dyn Write, &mut dyn Write) -> i32) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = f(&mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn stop_when_nothing_runs_is_not_an_error() {
        let (_dir, paths) = home();
        let (code, out, err) = call(|o, e| stop(&paths, o, e));
        assert_eq!(code, 0);
        assert_eq!(out, "The worker is not running.\n");
        assert!(err.is_empty());
        assert!(!paths.dir.exists(), "asking creates nothing");
    }

    #[test]
    fn stop_when_nothing_runs_mentions_a_give_up() {
        let (_dir, paths) = home();
        paths.ensure_dirs().unwrap();
        let s = crate::worker::state::SupervisorState {
            schema: crate::worker::state::SUPERVISOR_STATE_SCHEMA,
            supervisor_pid: 1,
            started_at: 0,
            phase: SupervisorPhase::GaveUp,
            worker_pid: None,
            worker_started_at: None,
            restarts: 4,
            last_exit: None,
            gave_up_reason: Some("5 crashes within 10 minutes".to_string()),
            backoff_until: None,
        };
        state::write_atomic(&paths.supervisor_state, &s).unwrap();
        let (code, out, _) = call(|o, e| stop(&paths, o, e));
        assert_eq!(code, 0);
        assert!(
            out.contains("last gave up: 5 crashes within 10 minutes"),
            "{out}"
        );
    }

    #[test]
    fn stop_with_a_held_lock_but_no_pid_file_fails() {
        let (_dir, paths) = home();
        let _held = lock::acquire(&RealFs, &paths.run_lock)
            .unwrap()
            .expect("free");
        let (code, _, err) = call(|o, e| stop(&paths, o, e));
        assert_eq!(code, 1);
        assert!(err.contains("has no pid"), "{err}");
    }

    #[test]
    fn stop_signals_a_foreground_worker_and_waits_for_its_lock() {
        let (_dir, paths) = home();
        paths.ensure_dirs().unwrap();
        // A child standing in for `worker run`: it exits on SIGTERM; the
        // test thread drops the lock once it has, as the OS would.
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 20"])
            .spawn()
            .unwrap();
        std::fs::write(&paths.worker_pid, format!("{}\n", child.id())).unwrap();
        let held = lock::acquire(&RealFs, &paths.run_lock)
            .unwrap()
            .expect("free");
        let releaser = std::thread::spawn(move || {
            let status = child.wait().unwrap();
            drop(held);
            status
        });
        let (code, out, err) = call(|o, e| stop(&paths, o, e));
        assert_eq!(code, 0, "{err}");
        assert!(out.ends_with("The worker stopped.\n"), "{out}");
        let status = releaser.join().unwrap();
        assert!(!status.success(), "it was signalled");
    }

    #[test]
    fn start_refuses_while_a_worker_runs() {
        let (_dir, paths) = home();
        paths.ensure_dirs().unwrap();
        std::fs::write(&paths.worker_pid, "4242\n").unwrap();
        let _held = lock::acquire(&RealFs, &paths.run_lock)
            .unwrap()
            .expect("free");
        let (code, out, err) = call(|o, e| start(&paths, o, e));
        assert_eq!(code, 1);
        assert!(out.is_empty());
        assert!(
            err.contains("already running for this account (pid 4242)"),
            "{err}"
        );
        assert!(!paths.supervisor_state.exists(), "nothing was spawned");
    }

    #[test]
    fn logs_prints_the_last_lines() {
        let (_dir, paths) = home();
        std::fs::create_dir_all(&paths.logs_dir).unwrap();
        std::fs::write(&paths.log_file, "a\nb\nc\n").unwrap();
        let done = AtomicBool::new(true);
        let (code, out, _) = call(|o, e| logs(&paths, 2, &done, o, e));
        assert_eq!(code, 0);
        assert_eq!(out, "b\nc\n");
    }

    #[test]
    fn logs_without_a_log_says_so() {
        let (_dir, paths) = home();
        let done = AtomicBool::new(true);
        let (code, out, _) = call(|o, e| logs(&paths, 100, &done, o, e));
        assert_eq!(code, 0);
        assert!(out.starts_with("No worker log yet"), "{out}");
    }
}
