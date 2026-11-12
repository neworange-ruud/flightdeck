//! `GET /download/{ticket}` against a worker's door faked on localhost: the
//! bytes land whole and atomically, a refusal's sentence is kept, and a short
//! body, a refusal or a cancel never leaves a file — temporary or not.

use std::io::Read;
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::*;
use crate::web::client::exchange::AccessToken;

/// What the door answers one request with.
enum Reply {
    /// These bytes, then hang up.
    Raw(Vec<u8>),
    /// These bytes, then hold the connection open until told (or dropped).
    Hold(Vec<u8>, mpsc::Receiver<()>),
}

fn ok(body: &[u8]) -> Reply {
    let mut raw = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\
         Content-Disposition: attachment; filename=\"x\"\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    raw.extend_from_slice(body);
    Reply::Raw(raw)
}

fn refused(status: &str, reason: &str, message: &str) -> Reply {
    let body = format!(r#"{{"ok":false,"reason":"{reason}","message":"{message}"}}"#);
    Reply::Raw(
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes(),
    )
}

/// A worker's download door on localhost answering one request per reply,
/// in order. Each request's head comes back on the channel.
fn door(replies: Vec<Reply>) -> (HttpAccess, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for reply in replies {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
            }
            let _ = tx.send(String::from_utf8_lossy(&raw).into_owned());
            match reply {
                Reply::Raw(bytes) => {
                    let _ = stream.write_all(&bytes);
                }
                Reply::Hold(bytes, until) => {
                    let _ = stream.write_all(&bytes);
                    let _ = stream.flush();
                    let _ = until.recv();
                }
            }
        }
    });
    let access = HttpAccess {
        address,
        token: AccessToken::new("secret-token"),
        user_agent: "FlightDeck Desktop test".to_string(),
    };
    (access, rx)
}

fn job(dest: &Path, final_name: &str, replace: bool) -> DownloadJob {
    DownloadJob {
        index: 0,
        ticket: "tk".to_string(),
        dest: dest.to_path_buf(),
        name: "a.txt".to_string(),
        final_name: final_name.to_string(),
        replace,
        size: 0,
    }
}

/// Every name in `dir`, sorted.
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn wait_for(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[tokio::test]
async fn the_file_lands_whole_under_its_name_and_nothing_else_is_left() {
    let body: Vec<u8> = (0..(CHUNK_BYTES * 3 + 17))
        .map(|i| (i % 251) as u8)
        .collect();
    let (access, heads) = door(vec![ok(&body)]);
    let dest = tempfile::tempdir().unwrap();
    let mut seen = Vec::new();
    let done = get_file(&access, &job(dest.path(), "a.txt", false), |got, total| {
        seen.push((got, total))
    })
    .await
    .unwrap();
    assert_eq!(done.path, dest.path().join("a.txt"));
    assert_eq!(done.bytes, body.len() as u64);
    assert_eq!(std::fs::read(&done.path).unwrap(), body);
    assert_eq!(listing(dest.path()), ["a.txt"], "no temporary file left");
    let total = body.len() as u64;
    assert_eq!(seen.last(), Some(&(total, total)));

    let head = heads.recv().unwrap();
    assert!(head.starts_with("GET /download/tk HTTP/1.1\r\n"), "{head}");
    assert!(head.contains("Cookie: flightdeck_web=secret-token"));
    assert!(head.contains("User-Agent: FlightDeck Desktop test"));
}

#[tokio::test]
async fn an_empty_file_lands_too() {
    let (access, _heads) = door(vec![ok(b"")]);
    let dest = tempfile::tempdir().unwrap();
    let done = get_file(&access, &job(dest.path(), "a.txt", false), |_, _| {})
        .await
        .unwrap();
    assert_eq!(std::fs::read(done.path).unwrap(), b"");
    assert_eq!(listing(dest.path()), ["a.txt"]);
}

#[tokio::test]
async fn a_refusal_keeps_the_workers_sentence_and_writes_nothing() {
    let (access, _heads) = door(vec![
        refused("403 Forbidden", "invalid_ticket", "That download expired; ask again."),
        refused("409 Conflict", "file_changed", "a.txt changed on the worker."),
        refused("500 Internal Server Error", "read_failed", "Could not read a.txt."),
        Reply::Raw(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 40\r\n\r\n{\"ok\":false,\"reason\":\"token_revoked\"}   ".to_vec()),
    ]);
    let dest = tempfile::tempdir().unwrap();
    let mut outcomes = Vec::new();
    for _ in 0..4 {
        let error = get_file(&access, &job(dest.path(), "a.txt", false), |_, _| {})
            .await
            .unwrap_err();
        outcomes.push(error);
    }
    assert_eq!(
        outcomes[0],
        DownloadError::Refused {
            status: 403,
            reason: "invalid_ticket".to_string(),
            message: Some("That download expired; ask again.".to_string()),
        }
    );
    assert_eq!(outcomes[1].sentence(), "a.txt changed on the worker.");
    assert_eq!(outcomes[2].sentence(), "Could not read a.txt.");
    assert_eq!(
        outcomes[3].sentence(),
        "The worker withdrew this app's access."
    );
    assert!(listing(dest.path()).is_empty(), "nothing written");
}

#[tokio::test]
async fn a_body_shorter_than_its_length_fails_and_leaves_nothing() {
    let mut raw = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n".to_vec();
    raw.extend_from_slice(b"only ten b");
    let (access, _heads) = door(vec![Reply::Raw(raw)]);
    let dest = tempfile::tempdir().unwrap();
    let error = get_file(&access, &job(dest.path(), "a.txt", false), |_, _| {})
        .await
        .unwrap_err();
    assert!(
        matches!(&error, DownloadError::Network(why) if why.contains("10 of 100")),
        "{error:?}"
    );
    assert!(error.sentence().starts_with("The download did not finish"));
    assert!(listing(dest.path()).is_empty(), "no partial file");
}

#[tokio::test]
async fn an_answer_with_no_length_is_refused() {
    let (access, _heads) = door(vec![Reply::Raw(b"HTTP/1.1 200 OK\r\n\r\nbytes".to_vec())]);
    let dest = tempfile::tempdir().unwrap();
    let error = get_file(&access, &job(dest.path(), "a.txt", false), |_, _| {})
        .await
        .unwrap_err();
    assert!(matches!(error, DownloadError::Network(_)), "{error:?}");
    assert!(listing(dest.path()).is_empty());
}

#[tokio::test]
async fn a_folder_that_cannot_be_written_is_a_local_failure() {
    let (access, _heads) = door(vec![ok(b"abc")]);
    let dest = tempfile::tempdir().unwrap();
    let gone = dest.path().join("missing");
    let error = get_file(&access, &job(&gone, "a.txt", false), |_, _| {})
        .await
        .unwrap_err();
    assert!(
        matches!(&error, DownloadError::Local(why) if why.contains("Could not save a.txt")),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_name_that_is_not_a_plain_file_name_is_never_asked_for() {
    let dest = tempfile::tempdir().unwrap();
    let access = HttpAccess {
        address: "127.0.0.1:9".to_string(),
        token: AccessToken::new("t"),
        user_agent: "t".to_string(),
    };
    for bad in ["../evil", "a/b", r"a\b", "..", ""] {
        let error = get_file(&access, &job(dest.path(), bad, false), |_, _| {})
            .await
            .unwrap_err();
        assert!(matches!(error, DownloadError::Local(_)), "{bad}: {error:?}");
    }
}

#[tokio::test]
async fn replace_overwrites_and_keep_both_takes_the_next_free_name() {
    let dest = tempfile::tempdir().unwrap();
    std::fs::write(dest.path().join("a.txt"), "old").unwrap();
    let (access, _heads) = door(vec![ok(b"new"), ok(b"both"), ok(b"third")]);

    // Replace: renamed over the old file.
    let done = get_file(&access, &job(dest.path(), "a.txt", true), |_, _| {})
        .await
        .unwrap();
    assert_eq!(done.path, dest.path().join("a.txt"));
    assert_eq!(
        std::fs::read_to_string(dest.path().join("a.txt")).unwrap(),
        "new"
    );

    // Not replacing, and a.txt is taken (it appeared since the panel
    // looked): the copy takes the next free numbered name.
    let done = get_file(&access, &job(dest.path(), "a.txt", false), |_, _| {})
        .await
        .unwrap();
    assert_eq!(done.path, dest.path().join("a (1).txt"));
    assert_eq!(std::fs::read_to_string(&done.path).unwrap(), "both");
    assert_eq!(
        std::fs::read_to_string(dest.path().join("a.txt")).unwrap(),
        "new"
    );

    // Keep both planned `a (1).txt`, which is now taken too: `a (2).txt`.
    let done = get_file(&access, &job(dest.path(), "a (1).txt", false), |_, _| {})
        .await
        .unwrap();
    assert_eq!(done.path, dest.path().join("a (2).txt"));
    assert_eq!(listing(dest.path()), ["a (1).txt", "a (2).txt", "a.txt"]);
}

#[test]
fn a_dropped_transfer_removes_its_temporary_file() {
    // The head and a first slice of the body, then the worker holds on.
    let mut raw = b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n".to_vec();
    raw.extend_from_slice(&[7u8; 4096]);
    let (release, held) = mpsc::channel();
    let (access, _heads) = door(vec![Reply::Hold(raw, held)]);
    let dest = tempfile::tempdir().unwrap();
    let (tx, events) = mpsc::channel();
    let transfer = DownloadTransfer::start(access, job(dest.path(), "a.txt", false), tx);
    wait_for("the temporary file", || {
        listing(dest.path())
            .iter()
            .any(|n| n.starts_with(".a.txt") && n.contains(TEMP_MARK))
    });
    wait_for("some progress", || {
        events
            .try_recv()
            .is_ok_and(|e| matches!(e, DownloadEvent::Progress { received, .. } if received > 0))
    });
    drop(transfer);
    wait_for("the temporary file to go", || {
        listing(dest.path()).is_empty()
    });
    assert!(
        !events
            .try_iter()
            .any(|e| matches!(e, DownloadEvent::Finished { .. })),
        "a cancelled transfer says nothing more"
    );
    drop(release);
}

#[test]
fn commit_never_overwrites_a_name_it_was_not_told_to_replace() {
    let dir = tempfile::tempdir().unwrap();
    let target = |final_name: &str, replace| Target {
        dir: dir.path().to_path_buf(),
        name: "f.bin".to_string(),
        final_name: final_name.to_string(),
        replace,
    };
    let temp = |body: &str| {
        let (path, mut file) = create_temp(dir.path(), "f.bin").unwrap();
        file.write_all(body.as_bytes()).unwrap();
        path
    };
    std::fs::write(dir.path().join("f.bin"), "keep").unwrap();
    let landed = commit(&temp("one"), &target("f.bin", false)).unwrap();
    assert_eq!(landed, dir.path().join("f (1).bin"));
    let landed = commit(&temp("two"), &target("f.bin", true)).unwrap();
    assert_eq!(landed, dir.path().join("f.bin"));
    assert_eq!(std::fs::read_to_string(&landed).unwrap(), "two");
    assert_eq!(listing(dir.path()), ["f (1).bin", "f.bin"]);
}
