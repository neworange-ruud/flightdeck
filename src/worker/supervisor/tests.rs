use super::*;

const SEC: u64 = 1000;
const MIN: u64 = 60 * SEC;

fn crash() -> ExitKind {
    ExitKind::Crashed {
        code: Some(101),
        signal: None,
    }
}

fn short() -> Duration {
    Duration::from_secs(2)
}

fn restart_after(secs: u64) -> Decision {
    Decision::Restart {
        after: Duration::from_secs(secs),
    }
}

#[test]
fn backoff_doubles_from_one_second_and_caps_at_sixty() {
    // A limit that never trips, so the cap shows.
    let policy = RestartPolicy {
        crash_limit: 100,
        ..RestartPolicy::default()
    };
    let mut t = CrashTracker::new(&policy);
    let backoffs: Vec<Decision> = (0..9)
        .map(|n| t.on_exit(&policy, n * SEC, short(), &crash(), false))
        .collect();
    let expected: Vec<Decision> = [1, 2, 4, 8, 16, 32, 60, 60, 60]
        .into_iter()
        .map(restart_after)
        .collect();
    assert_eq!(backoffs, expected);
}

#[test]
fn five_crashes_within_ten_minutes_give_up() {
    let policy = RestartPolicy::default();
    let mut t = CrashTracker::new(&policy);
    for (n, secs) in [1, 2, 4, 8].into_iter().enumerate() {
        assert_eq!(
            t.on_exit(&policy, n as u64 * MIN, short(), &crash(), false),
            restart_after(secs)
        );
    }
    assert_eq!(
        t.on_exit(&policy, 9 * MIN, short(), &crash(), false),
        Decision::GiveUp {
            reason: "5 crashes within 10 minutes".to_string()
        }
    );
}

#[test]
fn crashes_spread_over_more_than_the_window_keep_restarting() {
    let policy = RestartPolicy::default();
    let mut t = CrashTracker::new(&policy);
    // One every 3 minutes: never more than 4 inside any 10-minute window.
    for n in 0..20 {
        let d = t.on_exit(&policy, n * 3 * MIN, short(), &crash(), false);
        assert!(matches!(d, Decision::Restart { .. }), "crash {n}: {d:?}");
    }
}

#[test]
fn a_crash_exactly_one_window_old_no_longer_counts() {
    let policy = RestartPolicy::default();
    let mut t = CrashTracker::new(&policy);
    for at in [0, 1, 2, 3] {
        t.on_exit(&policy, at * MIN, short(), &crash(), false);
    }
    let d = t.on_exit(&policy, 10 * MIN, short(), &crash(), false);
    assert!(matches!(d, Decision::Restart { .. }), "{d:?}");
}

#[test]
fn a_stable_run_resets_the_backoff() {
    let policy = RestartPolicy::default();
    let mut t = CrashTracker::new(&policy);
    assert_eq!(
        t.on_exit(&policy, 0, short(), &crash(), false),
        restart_after(1)
    );
    assert_eq!(
        t.on_exit(&policy, 3 * SEC, short(), &crash(), false),
        restart_after(2)
    );
    assert_eq!(
        t.on_exit(&policy, 6 * SEC, short(), &crash(), false),
        restart_after(4)
    );
    // Ran 61 s before crashing: back to the first backoff, then doubling again.
    let long = Duration::from_secs(61);
    assert_eq!(
        t.on_exit(&policy, 2 * MIN, long, &crash(), false),
        restart_after(1)
    );
    assert_eq!(
        t.on_exit(&policy, 12 * MIN, short(), &crash(), false),
        restart_after(2)
    );
}

#[test]
fn any_exit_while_stopping_stops() {
    let policy = RestartPolicy::default();
    let mut t = CrashTracker::new(&policy);
    for exit in [
        ExitKind::Clean,
        crash(),
        ExitKind::Crashed {
            code: None,
            signal: Some(9),
        },
    ] {
        assert_eq!(t.on_exit(&policy, 0, short(), &exit, true), Decision::Stop);
    }
}

#[test]
fn a_clean_exit_nobody_asked_for_is_a_crash() {
    let policy = RestartPolicy::default();
    let mut t = CrashTracker::new(&policy);
    assert_eq!(
        t.on_exit(&policy, 0, short(), &ExitKind::Clean, false),
        restart_after(1)
    );
}

fn fatal(code: i32) -> ExitKind {
    ExitKind::Fatal {
        code,
        reason: format!("fatal {code}"),
    }
}

#[test]
fn an_invalid_config_gives_up_at_once() {
    let policy = RestartPolicy::default();
    let mut t = CrashTracker::new(&policy);
    assert_eq!(
        t.on_exit(&policy, 0, short(), &fatal(EXIT_CONFIG_INVALID), false),
        Decision::GiveUp {
            reason: "fatal 78".to_string()
        }
    );
}

#[test]
fn already_running_is_retried_once_then_gives_up() {
    let policy = RestartPolicy::default();
    let mut t = CrashTracker::new(&policy);
    let already = fatal(EXIT_ALREADY_RUNNING);
    assert_eq!(
        t.on_exit(&policy, 0, short(), &already, false),
        restart_after(1),
        "a TUI probing run.lock at the wrong instant must not end the worker"
    );
    assert_eq!(
        t.on_exit(&policy, SEC, short(), &already, false),
        Decision::GiveUp {
            reason: "fatal 73".to_string()
        }
    );
}

#[test]
fn already_running_after_another_exit_gets_its_retry_again() {
    let policy = RestartPolicy::default();
    let mut t = CrashTracker::new(&policy);
    let already = fatal(EXIT_ALREADY_RUNNING);
    assert!(matches!(
        t.on_exit(&policy, 0, short(), &already, false),
        Decision::Restart { .. }
    ));
    assert!(matches!(
        t.on_exit(&policy, MIN, short(), &crash(), false),
        Decision::Restart { .. }
    ));
    assert!(matches!(
        t.on_exit(&policy, 2 * MIN, short(), &already, false),
        Decision::Restart { .. }
    ));
}

#[cfg(unix)]
#[test]
fn classify_exit_statuses() {
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;
    let code = |c: i32| ExitStatus::from_raw(c << 8);
    assert_eq!(classify(code(0)), ExitKind::Clean);
    assert!(matches!(
        classify(code(73)),
        ExitKind::Fatal { code: 73, .. }
    ));
    assert!(matches!(
        classify(code(78)),
        ExitKind::Fatal { code: 78, .. }
    ));
    assert_eq!(
        classify(code(101)),
        ExitKind::Crashed {
            code: Some(101),
            signal: None
        }
    );
    assert_eq!(
        classify(ExitStatus::from_raw(9)),
        ExitKind::Crashed {
            code: None,
            signal: Some(9)
        }
    );
}

#[test]
fn exits_are_described_for_a_person() {
    assert_eq!(describe(&ExitKind::Clean), "exited with code 0");
    assert_eq!(describe(&crash()), "exited with code 101 (panic)");
    assert_eq!(
        describe(&ExitKind::Crashed {
            code: Some(2),
            signal: None
        }),
        "exited with code 2"
    );
    assert_eq!(
        describe(&ExitKind::Crashed {
            code: None,
            signal: Some(9)
        }),
        "killed by signal 9"
    );
    assert_eq!(
        describe(&fatal(EXIT_CONFIG_INVALID)),
        "exited with code 78 (invalid configuration)"
    );
    assert_eq!(
        describe(&fatal(EXIT_ALREADY_RUNNING)),
        "exited with code 73 (another worker is running)"
    );
}

#[test]
fn durations_read_naturally() {
    assert_eq!(describe_duration(Duration::from_secs(600)), "10 minutes");
    assert_eq!(describe_duration(Duration::from_secs(60)), "1 minute");
    assert_eq!(describe_duration(Duration::from_secs(1)), "1 second");
    assert_eq!(describe_duration(Duration::from_secs(90)), "90 seconds");
}

#[cfg(unix)]
mod lifecycle {
    //! The real loop over `/bin/sh` children standing in for `worker run`.

    use super::*;
    use std::sync::Arc;

    fn quick(crash_limit: u32) -> Tuning {
        Tuning {
            policy: RestartPolicy {
                initial: Duration::from_millis(10),
                max: Duration::from_millis(40),
                crash_limit,
                window: Duration::from_secs(600),
                stable_after: Duration::from_secs(60),
            },
            stop_grace: Duration::from_millis(300),
            poll: Duration::from_millis(10),
        }
    }

    fn home() -> (tempfile::TempDir, WorkerPaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = WorkerPaths::from_home(dir.path());
        paths.ensure_dirs().unwrap();
        (dir, paths)
    }

    fn sh() -> &'static Path {
        Path::new("/bin/sh")
    }

    #[test]
    fn a_crashing_worker_is_restarted_then_given_up_on_with_its_output_logged() {
        let (_dir, paths) = home();
        let stop = AtomicBool::new(false);
        let code = supervise_with(
            &paths,
            sh(),
            &["-c", "echo from-the-worker; echo oops >&2; exit 3"],
            &quick(3),
            &stop,
        )
        .unwrap();
        assert_eq!(code, 1);
        let s = state::read(&paths.supervisor_state).unwrap();
        assert_eq!(s.phase, SupervisorPhase::GaveUp);
        assert_eq!(
            s.gave_up_reason.as_deref(),
            Some("3 crashes within 10 minutes")
        );
        assert_eq!(s.restarts, 2);
        assert_eq!(s.worker_pid, None);
        let last = s.last_exit.unwrap();
        assert_eq!((last.code, last.signal), (Some(3), None));
        assert_eq!(last.description, "exited with code 3");
        assert!(!paths.supervisor_pid.exists(), "pid file removed on exit");

        let log = std::fs::read_to_string(&paths.log_file).unwrap();
        assert!(log.contains("from-the-worker\n"), "{log}");
        assert!(log.contains("oops\n"), "{log}");
        assert!(log.contains("] warn worker exited with code 3"), "{log}");
        assert!(
            log.contains("] error giving up: 3 crashes within 10 minutes"),
            "{log}"
        );
        assert!(log.contains(&format!("supervisor[{}]", std::process::id())));
    }

    #[test]
    fn an_invalid_config_exit_gives_up_without_a_restart() {
        let (_dir, paths) = home();
        let stop = AtomicBool::new(false);
        let code = supervise_with(&paths, sh(), &["-c", "exit 78"], &quick(5), &stop).unwrap();
        assert_eq!(code, 1);
        let s = state::read(&paths.supervisor_state).unwrap();
        assert_eq!(s.phase, SupervisorPhase::GaveUp);
        assert_eq!(s.restarts, 0);
        assert!(s.gave_up_reason.unwrap().contains("config is invalid"));
    }

    #[test]
    fn a_second_supervisor_refuses_to_run() {
        let (_dir, paths) = home();
        let _held = lock::acquire(&RealFs, &paths.supervisor_lock)
            .unwrap()
            .expect("free");
        let stop = AtomicBool::new(false);
        let code = supervise_with(&paths, sh(), &["-c", "exit 0"], &quick(5), &stop).unwrap();
        assert_eq!(code, 1);
        assert!(
            state::read(&paths.supervisor_state).is_none(),
            "the running supervisor's state is left alone"
        );
    }

    fn stop_after(stop: &Arc<AtomicBool>, after: Duration) {
        let stop = Arc::clone(stop);
        std::thread::spawn(move || {
            std::thread::sleep(after);
            stop.store(true, Ordering::Relaxed);
        });
    }

    #[test]
    fn stop_terminates_the_worker_and_records_stopped() {
        let (_dir, paths) = home();
        let stop = Arc::new(AtomicBool::new(false));
        stop_after(&stop, Duration::from_millis(200));
        let started = Instant::now();
        let code = supervise_with(&paths, sh(), &["-c", "sleep 20"], &quick(5), &stop).unwrap();
        assert_eq!(code, 0);
        assert!(started.elapsed() < Duration::from_secs(10));
        let s = state::read(&paths.supervisor_state).unwrap();
        assert_eq!(s.phase, SupervisorPhase::Stopped);
        assert_eq!(s.restarts, 0);
        assert_eq!(s.worker_pid, None);
        let log = std::fs::read_to_string(&paths.log_file).unwrap();
        assert!(log.contains("] info stopping worker["), "{log}");
        assert!(log.contains("] info stopped"), "{log}");
    }

    #[test]
    fn a_worker_that_ignores_sigterm_is_killed_after_the_grace() {
        let (_dir, paths) = home();
        let stop = Arc::new(AtomicBool::new(false));
        stop_after(&stop, Duration::from_millis(200));
        let code = supervise_with(
            &paths,
            sh(),
            &["-c", "trap '' TERM; while :; do sleep 1; done"],
            &quick(5),
            &stop,
        )
        .unwrap();
        assert_eq!(code, 0);
        let s = state::read(&paths.supervisor_state).unwrap();
        assert_eq!(s.phase, SupervisorPhase::Stopped);
        assert_eq!(s.last_exit.unwrap().signal, Some(libc::SIGKILL));
        let log = std::fs::read_to_string(&paths.log_file).unwrap();
        assert!(log.contains("did not exit within"), "{log}");
    }

    #[test]
    fn stop_during_backoff_ends_the_wait() {
        let (_dir, paths) = home();
        let mut tuning = quick(5);
        tuning.policy.initial = Duration::from_secs(30);
        tuning.policy.max = Duration::from_secs(30);
        let stop = Arc::new(AtomicBool::new(false));
        stop_after(&stop, Duration::from_millis(300));
        let started = Instant::now();
        let code = supervise_with(&paths, sh(), &["-c", "exit 1"], &tuning, &stop).unwrap();
        assert_eq!(code, 0);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the 30 s backoff was cut short"
        );
        let s = state::read(&paths.supervisor_state).unwrap();
        assert_eq!(s.phase, SupervisorPhase::Stopped);
        assert_eq!(s.restarts, 1);
        assert_eq!(s.backoff_until, None);
    }

    #[test]
    fn a_program_that_cannot_start_counts_as_a_crash() {
        let (dir, paths) = home();
        let stop = AtomicBool::new(false);
        let missing = dir.path().join("no-such-flightdeck");
        let code = supervise_with(&paths, &missing, &[], &quick(2), &stop).unwrap();
        assert_eq!(code, 1);
        let s = state::read(&paths.supervisor_state).unwrap();
        assert_eq!(s.phase, SupervisorPhase::GaveUp);
        assert!(s
            .last_exit
            .unwrap()
            .description
            .starts_with("could not be started"),);
    }
}
