use super::*;
use crate::testing::FakeFs;
use std::cell::RefCell;

const PATH: &str = "/home/u/.flightdeck/config.toml";

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

/// Run `command` against `fs`, with a worker that answers `reply` and records
/// the list it was handed.
fn run_cmd(
    fs: &FakeFs,
    command: AgentsCommand,
    reply: Result<ControlResponse, ControlError>,
) -> (i32, String, String, Option<Vec<String>>) {
    let sent = RefCell::new(None);
    let notify = |agents: &[String]| {
        *sent.borrow_mut() = Some(agents.to_vec());
        reply.clone()
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_with(&command, fs, Path::new(PATH), &notify, &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
        sent.into_inner(),
    )
}

fn disabled(fs: &FakeFs) -> Vec<String> {
    let text = fs.file_contents(Path::new(PATH)).unwrap();
    parse_config(&text).unwrap().worker.disabled_agents
}

#[test]
fn parse_table() {
    let cases: Vec<(Vec<&str>, Result<AgentsCommand, ()>)> = vec![
        (vec![], Ok(AgentsCommand::List { json: false })),
        (vec!["--json"], Ok(AgentsCommand::List { json: true })),
        (
            vec!["disable", "codex"],
            Ok(AgentsCommand::Disable {
                name: "codex".to_string(),
            }),
        ),
        (
            vec!["enable", "cursor"],
            Ok(AgentsCommand::Enable {
                name: "cursor".to_string(),
            }),
        ),
        (vec!["disable"], Err(())),
        (vec!["disable", " "], Err(())),
        (vec!["remove", "codex"], Err(())),
        (vec!["disable", "codex", "cursor"], Err(())),
    ];
    for (args, want) in cases {
        assert_eq!(parse(&strings(&args)).map_err(|_| ()), want, "{args:?}");
    }
}

#[test]
fn everything_is_enabled_by_default_and_the_file_is_created() {
    let fs = FakeFs::new();
    let (code, out, _, sent) = run_cmd(
        &fs,
        AgentsCommand::List { json: false },
        Err(ControlError::NotRunning),
    );
    assert_eq!(code, 0);
    assert!(fs.file_contents(Path::new(PATH)).is_some());
    for key in ["claude", "codex", "cursor", "opencode"] {
        let row = out
            .lines()
            .find(|l| l.trim_start().starts_with(key))
            .unwrap_or_else(|| panic!("no row for {key}: {out}"));
        assert!(row.ends_with("enabled"), "{row}");
    }
    assert_eq!(sent, None, "listing never talks to the worker");
}

#[test]
fn disable_writes_the_list_keeps_comments_and_tells_the_worker() {
    let fs = FakeFs::new();
    run_cmd(
        &fs,
        AgentsCommand::List { json: false },
        Err(ControlError::NotRunning),
    );
    let before = fs.file_contents(Path::new(PATH)).unwrap();
    assert!(before.contains("# FlightDeck Worker"));

    let (code, out, err, sent) = run_cmd(
        &fs,
        AgentsCommand::Disable {
            name: "codex".to_string(),
        },
        Ok(ControlResponse::Ok),
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("Disabled"), "{out}");
    assert!(out.contains("The running worker applied it"), "{out}");
    assert_eq!(sent, Some(vec!["codex".to_string()]));
    assert_eq!(disabled(&fs), vec!["codex".to_string()]);
    let after = fs.file_contents(Path::new(PATH)).unwrap();
    assert!(after.contains("# FlightDeck Worker"), "comments survive");

    let (code, _, _, sent) = run_cmd(
        &fs,
        AgentsCommand::Disable {
            name: "cursor".to_string(),
        },
        Err(ControlError::NotRunning),
    );
    assert_eq!(code, 0);
    assert_eq!(sent, Some(vec!["codex".to_string(), "cursor".to_string()]));

    let (_, out, _, _) = run_cmd(
        &fs,
        AgentsCommand::List { json: false },
        Err(ControlError::NotRunning),
    );
    let row = |key: &str| {
        out.lines()
            .find(|l| l.trim_start().starts_with(key))
            .unwrap()
            .to_string()
    };
    assert!(row("codex").ends_with("disabled"));
    assert!(row("cursor").ends_with("disabled"));
    assert!(row("claude").ends_with("enabled"));
}

#[test]
fn enable_removes_it_again() {
    let fs = FakeFs::new();
    let disable = AgentsCommand::Disable {
        name: "codex".to_string(),
    };
    run_cmd(&fs, disable, Err(ControlError::NotRunning));
    let (code, out, _, sent) = run_cmd(
        &fs,
        AgentsCommand::Enable {
            name: "codex".to_string(),
        },
        Err(ControlError::NotRunning),
    );
    assert_eq!(code, 0);
    assert!(out.contains("it applies this when it starts"), "{out}");
    assert_eq!(sent, Some(Vec::new()));
    assert!(disabled(&fs).is_empty());
}

#[test]
fn an_already_set_agent_is_a_no_op() {
    let fs = FakeFs::new();
    let (code, out, _, sent) = run_cmd(
        &fs,
        AgentsCommand::Enable {
            name: "codex".to_string(),
        },
        Ok(ControlResponse::Ok),
    );
    assert_eq!(code, 0);
    assert!(out.contains("already enabled"), "{out}");
    assert_eq!(sent, None);
}

#[test]
fn an_unknown_agent_is_refused() {
    let fs = FakeFs::new();
    let (code, _, err, sent) = run_cmd(
        &fs,
        AgentsCommand::Disable {
            name: "copilot".to_string(),
        },
        Ok(ControlResponse::Ok),
    );
    assert_eq!(code, 2);
    assert!(err.contains("No agent named 'copilot'"), "{err}");
    assert!(err.contains("claude"), "lists the known ones: {err}");
    assert_eq!(sent, None);
}

#[test]
fn the_last_enabled_agent_cannot_be_disabled() {
    let fs = FakeFs::new();
    for name in ["codex", "cursor", "opencode"] {
        let (code, _, err, _) = run_cmd(
            &fs,
            AgentsCommand::Disable {
                name: name.to_string(),
            },
            Err(ControlError::NotRunning),
        );
        assert_eq!(code, 0, "{err}");
    }
    let (code, _, err, sent) = run_cmd(
        &fs,
        AgentsCommand::Disable {
            name: "claude".to_string(),
        },
        Ok(ControlResponse::Ok),
    );
    assert_eq!(code, 1);
    assert!(err.contains("last enabled agent"), "{err}");
    assert_eq!(sent, None);
    assert!(!disabled(&fs).contains(&"claude".to_string()));
}

#[test]
fn an_older_worker_is_told_to_restart() {
    assert!(applied_line(Err(ControlError::Protocol("x".into()))).contains("restart"));
    assert!(applied_line(Ok(ControlResponse::Error {
        message: "unknown".into()
    }))
    .contains("restart"));
}

#[test]
fn json_listing_names_every_agent() {
    let fs = FakeFs::new();
    run_cmd(
        &fs,
        AgentsCommand::Disable {
            name: "cursor".to_string(),
        },
        Err(ControlError::NotRunning),
    );
    let (code, out, _, _) = run_cmd(
        &fs,
        AgentsCommand::List { json: true },
        Err(ControlError::NotRunning),
    );
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let agents = v["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 4);
    let cursor = agents.iter().find(|a| a["name"] == "cursor").unwrap();
    assert_eq!(cursor["enabled"], false);
    assert_eq!(cursor["display_name"], "Cursor CLI");
    let claude = agents.iter().find(|a| a["name"] == "claude").unwrap();
    assert_eq!(claude["enabled"], true);
}

#[test]
fn set_disabled_creates_the_table_and_keeps_other_values() {
    let (text, list) = set_disabled("# mine\n[ui]\ntheme = \"dark\"\n", "codex", true).unwrap();
    assert_eq!(list, vec!["codex".to_string()]);
    assert!(text.starts_with("# mine\n"), "{text}");
    assert!(text.contains("theme = \"dark\""), "{text}");
    let table: toml::Table = text.parse().unwrap();
    assert_eq!(
        table["worker"]["disabled_agents"].as_array().unwrap().len(),
        1
    );

    let (text, list) = set_disabled(
        "[worker]\nport = 9000 # mine\ndisabled_agents = [\"codex\", \"cursor\"]\n",
        "codex",
        false,
    )
    .unwrap();
    assert_eq!(list, vec!["cursor".to_string()]);
    assert!(text.contains("port = 9000 # mine"), "{text}");
    assert!(set_disabled("[worker]\ndisabled_agents = 3\n", "codex", true).is_err());
}
