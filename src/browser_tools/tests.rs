use std::path::{Path, PathBuf};

use serde_json::json;

use super::bridge::{read_ticket, BrowserBridge};
use super::launch::{agent_args, BrowserMcpLaunch};
use super::tools::{catalog, Tool};
use super::*;
use crate::contracts::{AgentDef, FileSystem, StatusPatterns};
use crate::testing::FakeFs;

fn agent(command: &str) -> AgentDef {
    AgentDef {
        key: "a".to_string(),
        display_name: "A".to_string(),
        command: command.to_string(),
        args: Vec::new(),
        status_patterns: StatusPatterns::default(),
    }
}

fn launch() -> BrowserMcpLaunch {
    BrowserMcpLaunch {
        command: "/opt/Flight Deck/flightdeck".to_string(),
        prefix: vec!["browser-mcp".to_string()],
        ticket: PathBuf::from("/home/u/.flightdeck/browser/worker.json"),
    }
}

#[test]
fn every_tool_in_the_catalog_parses_and_names_are_unique() {
    let tools = catalog();
    let mut names: Vec<&str> = tools.iter().map(|t| t.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), tools.len());
    for tool in &tools {
        // The required arguments, each as a string.
        let mut args = serde_json::Map::new();
        for key in tool.input_schema["required"].as_array().unwrap() {
            args.insert(key.as_str().unwrap().to_string(), json!("x"));
        }
        assert!(
            Tool::parse(tool.name, &serde_json::Value::Object(args)).is_ok(),
            "{} does not parse with its required arguments",
            tool.name
        );
        assert_eq!(tool.input_schema["type"], "object");
    }
}

#[test]
fn a_call_is_parsed_with_its_tab_and_flags() {
    assert_eq!(
        Tool::parse(
            "browser_type",
            &json!({"ref": "e3", "text": "hi", "submit": true, "tab": 2})
        ),
        Ok(Tool::Type {
            r#ref: "e3".to_string(),
            text: "hi".to_string(),
            submit: true,
            tab: Some(2)
        })
    );
    assert_eq!(
        Tool::parse("browser_snapshot", &json!({})),
        Ok(Tool::Snapshot { tab: None })
    );
    assert!(Tool::parse("browser_click", &json!({})).is_err());
    assert!(Tool::parse("browser_back", &json!({"tab": "one"})).is_err());
    assert!(Tool::parse("browser_teleport", &json!({})).is_err());
}

#[test]
fn the_mcp_server_initializes_and_lists_its_tools() {
    let init = mcp::handle(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}
    }))
    .unwrap();
    assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(init["result"]["serverInfo"]["name"], MCP_SERVER_NAME);
    assert!(init["result"]["capabilities"]["tools"].is_object());

    let unknown = mcp::handle(&json!({
        "jsonrpc": "2.0", "id": 2, "method": "initialize",
        "params": {"protocolVersion": "1999-01-01"}
    }))
    .unwrap();
    assert_eq!(unknown["result"]["protocolVersion"], "2025-06-18");

    let list = mcp::handle(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"})).unwrap();
    let tools = list["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), catalog().len());
    assert!(tools.iter().all(|t| t["inputSchema"].is_object()));

    // A notification is never answered; an unknown method is an error.
    assert!(
        mcp::handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none()
    );
    let nope =
        mcp::handle(&json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"})).unwrap();
    assert_eq!(nope["error"]["code"], -32601);
}

#[test]
fn a_reply_becomes_an_mcp_result_with_images() {
    let result = mcp::result_json(&BrowserReply::png("AAAA".to_string(), "Screenshot"));
    assert_eq!(result["isError"], false);
    assert_eq!(result["content"][0]["type"], "text");
    assert_eq!(result["content"][1]["type"], "image");
    assert_eq!(result["content"][1]["mimeType"], "image/png");
    assert_eq!(
        mcp::result_json(&BrowserReply::error("no"))["isError"],
        true
    );
}

#[test]
fn browser_mcp_takes_a_ticket_and_a_session() {
    let args: Vec<String> = ["--ticket", "/t.json", "--session", "s-1"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        mcp::parse_args(&args),
        Ok((PathBuf::from("/t.json"), "s-1".to_string()))
    );
    assert!(mcp::parse_args(&args[..2]).is_err());
    assert!(mcp::parse_args(&["--x".to_string()]).is_err());
}

#[test]
fn claude_gets_an_mcp_config_file_naming_the_server() {
    let fs = FakeFs::default();
    let root = Path::new("/w/feature");
    let args = agent_args(&fs, &agent("claude"), root, &launch(), "feat-1").unwrap();
    assert_eq!(args[0], "--mcp-config");
    let file = PathBuf::from(&args[1]);
    assert!(file.starts_with(root.join(".flightdeck/runtime/status")));
    let config: serde_json::Value =
        serde_json::from_str(&fs.read_to_string(&file).unwrap()).unwrap();
    let server = &config["mcpServers"][MCP_SERVER_NAME];
    assert_eq!(server["command"], "/opt/Flight Deck/flightdeck");
    assert_eq!(
        server["args"],
        json!([
            "browser-mcp",
            "--ticket",
            "/home/u/.flightdeck/browser/worker.json",
            "--session",
            "feat-1"
        ])
    );
}

#[test]
fn codex_gets_config_overrides_whose_values_are_toml() {
    let fs = FakeFs::default();
    let mut l = launch();
    // A Windows path's backslashes and a quote survive TOML quoting.
    l.command = r#"C:\Program Files\Flight"Deck\flightdeck.exe"#.to_string();
    let args = agent_args(&fs, &agent("codex"), Path::new("/w"), &l, "s").unwrap();
    assert_eq!(args.len(), 4);
    assert_eq!(args[0], "-c");
    let (key, value) = args[1].split_once('=').unwrap();
    assert_eq!(key, format!("mcp_servers.{MCP_SERVER_NAME}.command"));
    let parsed: toml::Value = toml::from_str(&format!("v = {value}")).unwrap();
    assert_eq!(parsed["v"].as_str(), Some(l.command.as_str()));
    let (key, value) = args[3].split_once('=').unwrap();
    assert_eq!(key, format!("mcp_servers.{MCP_SERVER_NAME}.args"));
    let parsed: toml::Value = toml::from_str(&format!("v = {value}")).unwrap();
    assert_eq!(parsed["v"].as_array().unwrap().len(), 5);
}

#[test]
fn opencode_cursor_and_unknown_agents_get_nothing() {
    let fs = FakeFs::default();
    for command in ["opencode", "cursor-agent", "aider"] {
        let args = agent_args(&fs, &agent(command), Path::new("/w"), &launch(), "s").unwrap();
        assert!(args.is_empty(), "{command}");
    }
}

#[test]
fn a_call_reaches_the_host_through_the_bridge_and_is_answered() {
    let dir = tempfile::tempdir().unwrap();
    let ticket = dir.path().join("browser").join("test.json");
    let bridge = BrowserBridge::start(ticket.clone()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&ticket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "only this account reads the ticket");
    }
    let caller = {
        let ticket = ticket.clone();
        std::thread::spawn(move || mcp::call(&ticket, "s-1", "browser_tabs".to_string(), json!({})))
    };
    // The host's turn loop: take the call, answer it.
    let call = loop {
        if let Some(call) = bridge.take_calls().pop() {
            break call;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_eq!(call.session, "s-1");
    assert_eq!(call.tool, "browser_tabs");
    assert!(bridge.answer(call.call, BrowserReply::text("tab 1")));
    assert_eq!(caller.join().unwrap(), BrowserReply::text("tab 1"));
    // Answered once only.
    assert!(!bridge.answer(call.call, BrowserReply::text("again")));
}

#[test]
fn a_request_with_the_wrong_token_is_refused_and_never_reaches_the_host() {
    let dir = tempfile::tempdir().unwrap();
    let ticket = dir.path().join("t.json");
    let bridge = BrowserBridge::start(ticket.clone()).unwrap();
    // A forged ticket: the right address, another token.
    let mut forged = read_ticket(&ticket).unwrap();
    forged.token = "guess".to_string();
    let fake = dir.path().join("forged.json");
    std::fs::write(&fake, serde_json::to_string(&forged).unwrap()).unwrap();
    let reply = mcp::call(&fake, "s", "browser_tabs".to_string(), json!({}));
    assert!(reply.is_error);
    assert!(bridge.take_calls().is_empty());
}

#[test]
fn dropping_the_bridge_removes_its_ticket_and_callers_hear_it_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let ticket = dir.path().join("t.json");
    let bridge = BrowserBridge::start(ticket.clone()).unwrap();
    drop(bridge);
    assert!(!ticket.exists());
    let reply = mcp::call(&ticket, "s", "browser_tabs".to_string(), json!({}));
    assert!(reply.is_error);
    assert!(reply.joined_text().contains("not running"));
}
