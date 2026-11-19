//! The tool catalog (`specs/SESSION_BROWSER_PRD.md` §6) and one call,
//! parsed.
//!
//! Every tool but `browser_tabs` and `browser_open` acts on one of the
//! session's browser tabs: the one its `tab` argument names, or else the
//! tab the agent used last, or else the one on screen, or else the newest.
//! Which one that is, is the window's to decide; it knows the tabs.

use serde_json::{json, Value};

/// One tool as MCP's `tools/list` describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

/// A browser tab, as a tool argument.
fn tab_arg() -> Value {
    json!({
        "type": "integer",
        "description": "The browser tab (from browser_tabs). Default: the tab you used last, else the one on screen, else the newest."
    })
}

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// Every tool, in the order an agent should read them.
pub fn catalog() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "browser_tabs",
            description: "List this session's browser tabs in FlightDeck: id, title, address, and which one the user has on screen.",
            input_schema: schema(json!({}), &[]),
        },
        ToolSpec {
            name: "browser_open",
            description: "Open a new browser tab in this session and load an address in it, like localhost:3000. Every request is made by the machine the session runs on, so localhost is that machine's. Answers once the page has loaded.",
            input_schema: schema(
                json!({ "url": { "type": "string", "description": "The address, e.g. localhost:3000 or https://example.com" } }),
                &["url"],
            ),
        },
        ToolSpec {
            name: "browser_navigate",
            description: "Load an address in a browser tab. Answers once the page has loaded.",
            input_schema: schema(
                json!({
                    "url": { "type": "string", "description": "The address, e.g. localhost:3000/login" },
                    "tab": tab_arg(),
                }),
                &["url"],
            ),
        },
        ToolSpec {
            name: "browser_back",
            description: "Go back one page in a browser tab.",
            input_schema: schema(json!({ "tab": tab_arg() }), &[]),
        },
        ToolSpec {
            name: "browser_reload",
            description: "Reload the page in a browser tab.",
            input_schema: schema(json!({ "tab": tab_arg() }), &[]),
        },
        ToolSpec {
            name: "browser_snapshot",
            description: "Read the page in a browser tab as a compact outline: headings, text, links, buttons and form fields, each element you can act on marked [ref=eN]. Use the refs with browser_click and browser_type. Refs change when the page does: take a new snapshot after acting.",
            input_schema: schema(json!({ "tab": tab_arg() }), &[]),
        },
        ToolSpec {
            name: "browser_click",
            description: "Click an element by its ref from browser_snapshot.",
            input_schema: schema(
                json!({
                    "ref": { "type": "string", "description": "The element's ref, e.g. e12" },
                    "tab": tab_arg(),
                }),
                &["ref"],
            ),
        },
        ToolSpec {
            name: "browser_type",
            description: "Type text into a form field by its ref from browser_snapshot, replacing what is in it.",
            input_schema: schema(
                json!({
                    "ref": { "type": "string", "description": "The field's ref, e.g. e7" },
                    "text": { "type": "string", "description": "The text to put in the field" },
                    "submit": { "type": "boolean", "description": "Press Enter afterwards (submits the form)" },
                    "tab": tab_arg(),
                }),
                &["ref", "text"],
            ),
        },
        ToolSpec {
            name: "browser_press",
            description: "Press a key in the page, on the element that has focus: Enter, Escape, Tab, Backspace, ArrowDown, a letter…",
            input_schema: schema(
                json!({
                    "key": { "type": "string", "description": "The key, as KeyboardEvent.key names it: Enter, Escape, ArrowDown, a" },
                    "tab": tab_arg(),
                }),
                &["key"],
            ),
        },
        ToolSpec {
            name: "browser_screenshot",
            description: "Take a screenshot of the visible part of the page in a browser tab.",
            input_schema: schema(json!({ "tab": tab_arg() }), &[]),
        },
        ToolSpec {
            name: "browser_console",
            description: "Read the page's console messages, uncaught errors and failed promises since the page loaded (or since the last clear).",
            input_schema: schema(
                json!({
                    "clear": { "type": "boolean", "description": "Empty the list after reading it" },
                    "tab": tab_arg(),
                }),
                &[],
            ),
        },
        ToolSpec {
            name: "browser_eval",
            description: "Run JavaScript in the page and answer its result as JSON: an expression, or a function like () => document.title. A promise is awaited.",
            input_schema: schema(
                json!({
                    "expression": { "type": "string", "description": "A JavaScript expression or function" },
                    "tab": tab_arg(),
                }),
                &["expression"],
            ),
        },
    ]
}

/// One call, parsed. `tab` is the tab the agent named, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tool {
    Tabs,
    Open {
        url: String,
    },
    Navigate {
        url: String,
        tab: Option<u64>,
    },
    Back {
        tab: Option<u64>,
    },
    Reload {
        tab: Option<u64>,
    },
    Snapshot {
        tab: Option<u64>,
    },
    Click {
        r#ref: String,
        tab: Option<u64>,
    },
    Type {
        r#ref: String,
        text: String,
        submit: bool,
        tab: Option<u64>,
    },
    Press {
        key: String,
        tab: Option<u64>,
    },
    Screenshot {
        tab: Option<u64>,
    },
    Console {
        clear: bool,
        tab: Option<u64>,
    },
    Eval {
        expression: String,
        tab: Option<u64>,
    },
}

impl Tool {
    /// Parse a call to `name` with `args`, or say what is wrong with it, in
    /// words for the agent.
    pub fn parse(name: &str, args: &Value) -> Result<Tool, String> {
        let tab = match args.get("tab") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_u64()
                    .ok_or_else(|| "tab must be a tab number from browser_tabs.".to_string())?,
            ),
        };
        let text = |key: &str| -> Result<String, String> {
            args.get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("{name} needs {key}."))
        };
        let flag = |key: &str| args.get(key).and_then(Value::as_bool).unwrap_or(false);
        Ok(match name {
            "browser_tabs" => Tool::Tabs,
            "browser_open" => Tool::Open { url: text("url")? },
            "browser_navigate" => Tool::Navigate {
                url: text("url")?,
                tab,
            },
            "browser_back" => Tool::Back { tab },
            "browser_reload" => Tool::Reload { tab },
            "browser_snapshot" => Tool::Snapshot { tab },
            "browser_click" => Tool::Click {
                r#ref: text("ref")?,
                tab,
            },
            "browser_type" => Tool::Type {
                r#ref: text("ref")?,
                text: text("text")?,
                submit: flag("submit"),
                tab,
            },
            "browser_press" => Tool::Press {
                key: text("key")?,
                tab,
            },
            "browser_screenshot" => Tool::Screenshot { tab },
            "browser_console" => Tool::Console {
                clear: flag("clear"),
                tab,
            },
            "browser_eval" => Tool::Eval {
                expression: text("expression")?,
                tab,
            },
            other => return Err(format!("There is no browser tool called {other}.")),
        })
    }

    /// The tab the agent named, for the tools that act on one.
    pub fn tab(&self) -> Option<u64> {
        match self {
            Tool::Tabs | Tool::Open { .. } => None,
            Tool::Navigate { tab, .. }
            | Tool::Back { tab }
            | Tool::Reload { tab }
            | Tool::Snapshot { tab }
            | Tool::Click { tab, .. }
            | Tool::Type { tab, .. }
            | Tool::Press { tab, .. }
            | Tool::Screenshot { tab }
            | Tool::Console { tab, .. }
            | Tool::Eval { tab, .. } => *tab,
        }
    }
}
