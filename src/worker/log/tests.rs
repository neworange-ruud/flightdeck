use std::sync::Arc;

use super::*;

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn open_creates_the_directory_and_appends_to_an_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("logs").join("worker.log");
    {
        let mut log = RotatingLog::open(&path, 1024, 5).unwrap();
        log.write_line("one");
    }
    let mut log = RotatingLog::open(&path, 1024, 5).unwrap();
    log.write_line("two\n");
    assert_eq!(
        read(&path),
        "one\ntwo\n",
        "a trailing newline is not doubled"
    );
}

#[test]
fn rotation_shifts_files_and_drops_the_oldest() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worker.log");
    // Each "line-N" is 7 bytes with its newline: two fit in 14, a third rotates.
    let mut log = RotatingLog::open(&path, 14, 3).unwrap();
    for n in 1..=8 {
        log.write_line(&format!("line-{n}"));
    }
    assert_eq!(read(&path), "line-7\nline-8\n");
    assert_eq!(read(&rotated_path(&path, 1)), "line-5\nline-6\n");
    assert_eq!(read(&rotated_path(&path, 2)), "line-3\nline-4\n");
    assert!(
        !rotated_path(&path, 3).exists(),
        "keep = 3 means the live file plus .1 and .2"
    );
}

#[test]
fn the_default_keep_leaves_five_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worker.log");
    let mut log = RotatingLog::open(&path, 7, LOG_KEEP).unwrap();
    for n in 1..=9 {
        log.write_line(&format!("line-{n}"));
    }
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "worker.log",
            "worker.log.1",
            "worker.log.2",
            "worker.log.3",
            "worker.log.4"
        ]
    );
    assert_eq!(read(&path), "line-9\n");
    assert_eq!(read(&rotated_path(&path, 4)), "line-5\n");
}

#[test]
fn an_existing_file_counts_toward_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worker.log");
    std::fs::write(&path, "0123456789\n").unwrap();
    let mut log = RotatingLog::open(&path, 14, 5).unwrap();
    log.write_line("next");
    assert_eq!(read(&path), "next\n");
    assert_eq!(read(&rotated_path(&path, 1)), "0123456789\n");
}

#[test]
fn a_line_longer_than_the_limit_is_kept_whole() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worker.log");
    let mut log = RotatingLog::open(&path, 4, 5).unwrap();
    log.write_line("a long line");
    assert_eq!(read(&path), "a long line\n", "an empty file never rotates");
    log.write_line("b");
    assert_eq!(read(&path), "b\n");
    assert_eq!(read(&rotated_path(&path, 1)), "a long line\n");
}

#[test]
fn format_line_shape() {
    assert_eq!(
        format_line("2026-10-23T10:00:00Z", "supervisor[42]", "info", "spawned"),
        "2026-10-23T10:00:00Z supervisor[42] info spawned"
    );
}

#[test]
fn tail_reads_the_end_of_the_current_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worker.log");
    std::fs::write(&path, "a\nb\nc\nd\n").unwrap();
    assert_eq!(tail(&path, 2).unwrap(), ["c", "d"]);
    assert_eq!(tail(&path, 0).unwrap(), Vec::<String>::new());
}

#[test]
fn tail_reaches_into_the_previous_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worker.log");
    let mut log = RotatingLog::open(&path, 14, 5).unwrap();
    for n in 1..=5 {
        log.write_line(&format!("line-{n}"));
    }
    // worker.log.1 = line-3, line-4; worker.log = line-5.
    assert_eq!(tail(&path, 2).unwrap(), ["line-4", "line-5"]);
    assert_eq!(
        tail(&path, 10).unwrap(),
        ["line-3", "line-4", "line-5"],
        "only `.1` is consulted"
    );
}

#[test]
fn tail_of_a_missing_log_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let err = tail(&dir.path().join("worker.log"), 5).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn tail_mid_rotation_reads_the_previous_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worker.log");
    std::fs::write(rotated_path(&path, 1), "x\ny\n").unwrap();
    assert_eq!(tail(&path, 5).unwrap(), ["x", "y"]);
}

#[test]
fn follow_copies_new_lines_across_rotations_until_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worker.log");
    let mut log = RotatingLog::open(&path, 21, 5).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let follower = {
        let (path, stop) = (path.clone(), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut out = Vec::new();
            follow_with(&path, &mut out, &stop, Duration::from_millis(5), true).unwrap();
            String::from_utf8(out).unwrap()
        })
    };
    let mut expected = String::new();
    for n in 1..=7 {
        let line = format!("line-{n}");
        log.write_line(&line);
        expected.push_str(&line);
        expected.push('\n');
        // Slower than the poll, so no rotation is skipped over entirely.
        std::thread::sleep(Duration::from_millis(50));
    }
    stop.store(true, Ordering::Relaxed);
    assert_eq!(follower.join().unwrap(), expected);
}

#[test]
fn follow_waits_for_a_log_that_does_not_exist_yet() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worker.log");
    let stop = Arc::new(AtomicBool::new(false));
    let follower = {
        let (path, stop) = (path.clone(), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut out = Vec::new();
            follow_with(&path, &mut out, &stop, Duration::from_millis(5), false).unwrap();
            String::from_utf8(out).unwrap()
        })
    };
    std::thread::sleep(Duration::from_millis(30));
    RotatingLog::open(&path, 1024, 5)
        .unwrap()
        .write_line("first");
    std::thread::sleep(Duration::from_millis(60));
    stop.store(true, Ordering::Relaxed);
    assert_eq!(
        follower.join().unwrap(),
        "first\n",
        "a log created after the follow began is read from its start"
    );
}

#[test]
fn follow_ends_on_a_failed_write() {
    struct Closed;
    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worker.log");
    std::fs::write(&path, "x\n").unwrap();
    let stop = AtomicBool::new(false);
    let err = follow_with(&path, &mut Closed, &stop, Duration::from_millis(5), true).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
}
