use super::*;
use serde_json::json;

#[test]
fn request_wire_shapes() {
    let cases = [
        (ControlRequest::Ping, json!({"op": "ping"})),
        (ControlRequest::Status, json!({"op": "status"})),
        (ControlRequest::Pair, json!({"op": "pair"})),
        (ControlRequest::Devices, json!({"op": "devices"})),
        (
            ControlRequest::Revoke {
                device: "mbp".to_string(),
            },
            json!({"op": "revoke", "device": "mbp"}),
        ),
        (
            ControlRequest::PrepareShutdown { restarting: true },
            json!({"op": "prepare_shutdown", "restarting": true}),
        ),
    ];
    for (req, wire) in cases {
        assert_eq!(serde_json::to_value(&req).unwrap(), wire);
        assert_eq!(serde_json::from_value::<ControlRequest>(wire).unwrap(), req);
    }
}

#[test]
fn response_wire_shapes() {
    let device = DeviceReport {
        id: "tok1".to_string(),
        label: "mbp".to_string(),
        address: None,
        paired_at: 10,
        last_seen: Some(20),
        state: DeviceState::Background,
        revoked: false,
    };
    let device_json = json!({
        "id": "tok1", "label": "mbp", "address": null, "paired_at": 10,
        "last_seen": 20, "state": "background", "revoked": false
    });
    let cases = [
        (
            ControlResponse::Pong {
                pid: 7,
                version: "1.0.0".to_string(),
            },
            json!({"result": "pong", "pid": 7, "version": "1.0.0"}),
        ),
        (
            ControlResponse::Paired(PairingCode {
                code: "1234".to_string(),
                expires_in_secs: 120,
                address: Some("box.ts.net:7420".to_string()),
                listening: true,
                not_listening_reason: None,
            }),
            json!({
                "result": "paired", "code": "1234", "expires_in_secs": 120,
                "address": "box.ts.net:7420", "listening": true,
                "not_listening_reason": null
            }),
        ),
        (
            ControlResponse::Devices {
                devices: vec![device.clone()],
            },
            json!({"result": "devices", "devices": [device_json.clone()]}),
        ),
        (
            ControlResponse::Revoked {
                device,
                closed_connections: 2,
            },
            json!({"result": "revoked", "device": device_json, "closed_connections": 2}),
        ),
        (ControlResponse::Ok, json!({"result": "ok"})),
        (
            ControlResponse::Error {
                message: "no such device".to_string(),
            },
            json!({"result": "error", "message": "no such device"}),
        ),
    ];
    for (resp, wire) in cases {
        assert_eq!(serde_json::to_value(&resp).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<ControlResponse>(wire).unwrap(),
            resp
        );
    }
}

#[test]
fn status_response_round_trips() {
    let resp = ControlResponse::Status(Box::new(crate::worker::status::tests::sample_runtime()));
    let wire = serde_json::to_value(&resp).unwrap();
    assert_eq!(wire["result"], "status");
    assert_eq!(
        serde_json::from_value::<ControlResponse>(wire).unwrap(),
        resp
    );
}

#[test]
fn a_reply_to_a_departed_client_is_dropped_quietly() {
    let (tx, rx) = std::sync::mpsc::channel();
    drop(rx);
    let call = ControlCall {
        request: ControlRequest::Ping,
        reply: tx,
    };
    call.reply(ControlResponse::Ok);
}

#[cfg(unix)]
mod transport {
    use super::super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::time::Instant;

    /// A directory with a short path: `sun_path` is 104 bytes on macOS, and
    /// the default temp dir there (`/var/folders/…`) eats most of it.
    fn short_dir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("fdc")
            .tempdir_in("/tmp")
            .unwrap()
    }

    fn socket_in(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("c.sock")
    }

    /// Answer every call with `answer(request)` until the server goes away.
    fn answer_with(
        calls: Receiver<ControlCall>,
        answer: impl Fn(&ControlRequest) -> ControlResponse + Send + 'static,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            while let Ok(call) = calls.recv() {
                let response = answer(&call.request);
                call.reply(response);
            }
        })
    }

    fn raw_exchange(socket: &Path, bytes: &[u8]) -> ControlResponse {
        let mut stream = UnixStream::connect(socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        // The server may stop reading (an oversized request) before the end.
        let _ = stream.write_all(bytes);
        let mut answer = String::new();
        stream.read_to_string(&mut answer).unwrap();
        serde_json::from_str(answer.trim_end()).unwrap()
    }

    #[test]
    fn a_request_round_trips_through_the_run_loop() {
        let dir = short_dir();
        let socket = socket_in(&dir);
        let (server, calls) = serve(&socket).unwrap();
        let loop_thread = answer_with(calls, |req| match req {
            ControlRequest::Revoke { device } => ControlResponse::Error {
                message: format!("no device '{device}'"),
            },
            _ => ControlResponse::Pong {
                pid: 7,
                version: "9.9.9".to_string(),
            },
        });

        let pong = request(&socket, &ControlRequest::Ping, Duration::from_secs(5)).unwrap();
        assert_eq!(
            pong,
            ControlResponse::Pong {
                pid: 7,
                version: "9.9.9".to_string()
            }
        );
        // One request per connection: a second one gets its own.
        let revoke = ControlRequest::Revoke {
            device: "mbp".to_string(),
        };
        assert_eq!(
            request(&socket, &revoke, Duration::from_secs(5)).unwrap(),
            ControlResponse::Error {
                message: "no device 'mbp'".to_string()
            }
        );

        drop(server);
        loop_thread.join().unwrap();
    }

    #[test]
    fn the_socket_is_private_to_the_account() {
        use std::os::unix::fs::PermissionsExt;
        let dir = short_dir();
        let socket = socket_in(&dir);
        let (_server, _calls) = serve(&socket).unwrap();
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn a_request_times_out_when_nobody_answers() {
        let dir = short_dir();
        let socket = socket_in(&dir);
        // The run loop holds the receiver but never serves it.
        let (_server, _calls) = serve(&socket).unwrap();
        let started = Instant::now();
        let err = request(&socket, &ControlRequest::Status, Duration::from_millis(200));
        assert_eq!(err, Err(ControlError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn the_server_answers_for_a_run_loop_that_is_too_slow() {
        let dir = short_dir();
        let socket = socket_in(&dir);
        let (_server, _calls) = serve_with(&socket, Duration::from_millis(100)).unwrap();
        assert_eq!(
            request(&socket, &ControlRequest::Status, Duration::from_secs(5)),
            Ok(ControlResponse::Error {
                message: "the worker did not answer in time".to_string()
            })
        );
    }

    #[test]
    fn a_run_loop_that_is_gone_means_shutting_down() {
        let dir = short_dir();
        let socket = socket_in(&dir);
        let (_server, calls) = serve(&socket).unwrap();
        drop(calls);
        assert_eq!(
            request(&socket, &ControlRequest::Pair, Duration::from_secs(5)),
            Ok(ControlResponse::Error {
                message: "the worker is shutting down".to_string()
            })
        );
    }

    #[test]
    fn nothing_listening_is_not_running() {
        let dir = short_dir();
        let socket = socket_in(&dir);
        assert_eq!(
            request(&socket, &ControlRequest::Ping, Duration::from_secs(1)),
            Err(ControlError::NotRunning)
        );
        // A socket file a dead worker left behind refuses connections.
        drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
        assert!(socket.exists());
        assert_eq!(
            request(&socket, &ControlRequest::Ping, Duration::from_secs(1)),
            Err(ControlError::NotRunning)
        );
    }

    #[test]
    fn a_stale_socket_file_is_replaced() {
        let dir = short_dir();
        let socket = socket_in(&dir);
        drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
        let (_server, calls) = serve(&socket).unwrap();
        let _loop = answer_with(calls, |_| ControlResponse::Ok);
        assert_eq!(
            request(&socket, &ControlRequest::Devices, Duration::from_secs(5)),
            Ok(ControlResponse::Ok)
        );
    }

    #[test]
    fn dropping_the_server_removes_the_socket() {
        let dir = short_dir();
        let socket = socket_in(&dir);
        let (server, _calls) = serve(&socket).unwrap();
        assert!(socket.exists());
        drop(server);
        assert!(!socket.exists());
        assert_eq!(
            request(&socket, &ControlRequest::Ping, Duration::from_secs(1)),
            Err(ControlError::NotRunning)
        );
    }

    #[test]
    fn a_line_that_is_not_a_request_gets_an_error_answer() {
        let dir = short_dir();
        let socket = socket_in(&dir);
        let (_server, calls) = serve(&socket).unwrap();
        let _loop = answer_with(calls, |_| ControlResponse::Ok);
        match raw_exchange(&socket, b"{\"op\":\"launch_missiles\"}\n") {
            ControlResponse::Error { message } => {
                assert!(message.starts_with("not a control request"), "{message}")
            }
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn an_oversized_request_is_refused() {
        let dir = short_dir();
        let socket = socket_in(&dir);
        let (_server, _calls) = serve(&socket).unwrap();
        let huge = vec![b'x'; unix::MAX_REQUEST_BYTES + 10];
        match raw_exchange(&socket, &huge) {
            ControlResponse::Error { message } => assert!(message.contains("longer than")),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn a_socket_path_over_the_platform_limit_is_refused_clearly() {
        let long = PathBuf::from(format!("/tmp/{}/control.sock", "d".repeat(120)));
        let err = check_socket_path(&long).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("byte limit"), "{err}");
        assert!(serve(&long).is_err());
        assert!(matches!(
            request(&long, &ControlRequest::Ping, Duration::from_secs(1)),
            Err(ControlError::Io(_))
        ));
        let fits = PathBuf::from(format!("/{}", "d".repeat(MAX_SOCKET_PATH_BYTES - 1)));
        assert!(check_socket_path(&fits).is_ok());
    }

    #[test]
    fn ping_reaches_a_worker_through_its_paths() {
        let dir = short_dir();
        let paths = WorkerPaths::from_home(dir.path());
        paths.ensure_dirs().unwrap();
        assert_eq!(ping(&paths), None);
        let (_server, calls) = serve(&paths.control_socket).unwrap();
        let _loop = answer_with(calls, |_| ControlResponse::Pong {
            pid: 42,
            version: "1.2.3".to_string(),
        });
        assert_eq!(ping(&paths), Some((42, "1.2.3".to_string())));
    }

    #[test]
    fn a_ping_answered_with_something_else_is_no_answer() {
        let dir = short_dir();
        let paths = WorkerPaths::from_home(dir.path());
        paths.ensure_dirs().unwrap();
        let (_server, calls) = serve(&paths.control_socket).unwrap();
        let _loop = answer_with(calls, |_| ControlResponse::Ok);
        assert_eq!(ping(&paths), None);
    }
}
