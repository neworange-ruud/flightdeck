//! `flightdeck worker agents`: which agent backends a worker offers (SPECS
//! §33).
//!
//! Every agent in the global `[agents]` table is offered by default. A machine
//! that has not set one up (no Codex, no Cursor) disables it here, and the
//! worker leaves it out of the New Agent form and the "+ agent" picker, so a
//! Desktop only offers what can start on this machine.
//!
//! The setting is `[worker] disabled_agents` in the global
//! `~/.flightdeck/config.toml`. `enable` / `disable` rewrite that one value
//! with `toml_edit`, keeping the file's comments and layout, then hand the new
//! list to a running worker over the control socket so it applies at once.
//! A stopped worker reads it when it starts.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::config::init::{backfill_builtin_agents, ensure_global_config};
use crate::config::load::{load_config, parse_config};
use crate::contracts::{Config, FileSystem, RealFs};
use crate::worker::control::{self, ControlError, ControlRequest, ControlResponse};
use crate::worker::paths::WorkerPaths;

/// How long `enable` / `disable` wait for a running worker to take the list.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);

/// A parsed `worker agents …` command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentsCommand {
    /// `agents [--json]`: list the backends and whether each is offered.
    List { json: bool },
    /// `agents enable <name>`.
    Enable { name: String },
    /// `agents disable <name>`.
    Disable { name: String },
}

/// Parse what follows `worker agents`; the error is the usage message.
pub fn parse(rest: &[String]) -> Result<AgentsCommand, String> {
    match rest {
        [] => Ok(AgentsCommand::List { json: false }),
        [flag] if flag == "--json" => Ok(AgentsCommand::List { json: true }),
        [verb, name] if verb == "enable" && !name.trim().is_empty() => Ok(AgentsCommand::Enable {
            name: name.trim().to_string(),
        }),
        [verb, name] if verb == "disable" && !name.trim().is_empty() => {
            Ok(AgentsCommand::Disable {
                name: name.trim().to_string(),
            })
        }
        _ => Err(
            "`flightdeck worker agents` takes `[--json]`, `enable <name>` or \
                  `disable <name>` (names as `flightdeck worker agents` lists them)."
                .to_string(),
        ),
    }
}

/// Run a `worker agents` command against this account's global config and
/// worker. Returns the exit code.
pub fn run(command: &AgentsCommand, paths: &WorkerPaths) -> i32 {
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    let notify = |agents: &[String]| {
        // The locks answer "is a worker running?", not the socket.
        if !crate::worker::lock::worker_running(&RealFs, paths) {
            return Err(ControlError::NotRunning);
        }
        control::request(
            &paths.control_socket,
            &ControlRequest::SetDisabledAgents {
                agents: agents.to_vec(),
            },
            REQUEST_TIMEOUT,
        )
    };
    run_with(
        command,
        &RealFs,
        &global_config_path(&paths.home),
        &notify,
        &mut out,
        &mut err,
    )
}

/// The global config a worker reads (SPECS §33).
pub fn global_config_path(home: &Path) -> PathBuf {
    home.join(".flightdeck").join("config.toml")
}

/// What telling the running worker about the new list came to.
type Notify<'a> = dyn Fn(&[String]) -> Result<ControlResponse, ControlError> + 'a;

/// [`run`] over explicit seams: the file system, the config path, how to
/// reach the worker, and the output streams.
fn run_with(
    command: &AgentsCommand,
    fs: &dyn FileSystem,
    path: &Path,
    notify: &Notify<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let config = match load(fs, path) {
        Ok(config) => config,
        Err(message) => {
            let _ = writeln!(err, "{message}");
            return 1;
        }
    };
    match command {
        AgentsCommand::List { json } => list(&config, path, *json, out, err),
        AgentsCommand::Enable { name } => set(&config, fs, path, name, false, notify, out, err),
        AgentsCommand::Disable { name } => set(&config, fs, path, name, true, notify, out, err),
    }
}

/// The global config, written with the documented defaults first when
/// missing, and with any built-in agent it lacks, as a starting worker does.
fn load(fs: &dyn FileSystem, path: &Path) -> Result<Config, String> {
    let created = ensure_global_config(fs, path)
        .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    if !created {
        let _ = backfill_builtin_agents(fs, path);
    }
    load_config(fs, path).map_err(|e| format!("Invalid global config {}: {e}", path.display()))
}

/// One backend, as `worker agents --json` prints it.
#[derive(Debug, Serialize)]
struct AgentReport<'a> {
    name: &'a str,
    display_name: &'a str,
    command: &'a str,
    enabled: bool,
}

/// `worker agents [--json]`.
fn list(config: &Config, path: &Path, json: bool, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let agents: Vec<AgentReport<'_>> = config
        .agents
        .iter()
        .map(|(key, def)| AgentReport {
            name: key,
            display_name: &def.display_name,
            command: &def.command,
            enabled: config.worker.agent_enabled(key),
        })
        .collect();
    if json {
        #[derive(Serialize)]
        struct Listing<'a> {
            config: String,
            agents: Vec<AgentReport<'a>>,
        }
        let listing = Listing {
            config: path.display().to_string(),
            agents,
        };
        return match serde_json::to_string_pretty(&listing) {
            Ok(text) => {
                let _ = writeln!(out, "{text}");
                0
            }
            Err(e) => {
                let _ = writeln!(err, "flightdeck worker: {e}");
                1
            }
        };
    }
    let _ = write!(out, "{}", render_list(&agents, path));
    0
}

/// The text listing: one row per backend, then how to change it.
fn render_list(agents: &[AgentReport<'_>], path: &Path) -> String {
    let name_w = agents.iter().map(|a| a.name.len()).max().unwrap_or(0);
    let display_w = agents
        .iter()
        .map(|a| a.display_name.chars().count())
        .max()
        .unwrap_or(0);
    let mut text = format!(
        "Agent backends a Desktop can start on this worker ({}):\n\n",
        path.display()
    );
    for a in agents {
        let state = if a.enabled { "enabled" } else { "disabled" };
        text.push_str(&format!(
            "  {:name_w$}  {:display_w$}  {state}\n",
            a.name, a.display_name
        ));
    }
    text.push_str(
        "\nDisable one this machine has not set up: flightdeck worker agents disable <name>\n\
         Enable it again:                          flightdeck worker agents enable <name>\n",
    );
    text
}

/// `worker agents enable|disable <name>`.
#[allow(clippy::too_many_arguments)]
fn set(
    config: &Config,
    fs: &dyn FileSystem,
    path: &Path,
    name: &str,
    disable: bool,
    notify: &Notify<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let listed = config.worker.disabled_agents.iter().any(|k| k == name);
    // A stale entry (an agent since removed from [agents]) may still be
    // enabled again, which only tidies the list.
    let tidying_stale_entry = listed && !disable;
    if !config.agents.contains_key(name) && !tidying_stale_entry {
        let known: Vec<&str> = config.agents.keys().map(String::as_str).collect();
        let _ = writeln!(
            err,
            "No agent named '{name}' in {}. Known agents: {}.",
            path.display(),
            known.join(", ")
        );
        return 2;
    }
    let display = config
        .agents
        .get(name)
        .map(|d| format!("{} ({name})", d.display_name))
        .unwrap_or_else(|| name.to_string());
    if listed == disable {
        let state = if disable { "disabled" } else { "enabled" };
        let _ = writeln!(out, "{display} is already {state}.");
        return 0;
    }
    if disable
        && !config
            .agents
            .keys()
            .any(|k| k != name && config.worker.agent_enabled(k))
    {
        let _ = writeln!(
            err,
            "Refused: {display} is the last enabled agent; a worker with none could not \
             start any session. Enable another one first."
        );
        return 1;
    }
    let contents = match fs.read_to_string(path) {
        Ok(contents) => contents,
        Err(e) => {
            let _ = writeln!(err, "Could not read {}: {e}", path.display());
            return 1;
        }
    };
    let (updated, agents) = match set_disabled(&contents, name, disable) {
        Ok(done) => done,
        Err(message) => {
            let _ = writeln!(err, "Could not update {}: {message}", path.display());
            return 1;
        }
    };
    // Never write a file the worker would then refuse to start with.
    if let Err(e) = parse_config(&updated) {
        let _ = writeln!(err, "Could not update {}: {e}", path.display());
        return 1;
    }
    if let Err(e) = fs.write(path, &updated) {
        let _ = writeln!(err, "Could not write {}: {e}", path.display());
        return 1;
    }
    let verb = if disable { "Disabled" } else { "Enabled" };
    let _ = writeln!(out, "{verb} {display} in {}.", path.display());
    let _ = writeln!(out, "{}", applied_line(notify(&agents)));
    0
}

/// What the CLI says about the running worker after the file changed.
fn applied_line(result: Result<ControlResponse, ControlError>) -> &'static str {
    match result {
        Ok(ControlResponse::Ok) => {
            "The running worker applied it: new sessions offer only the \
             enabled agents. Sessions already running keep running."
        }
        Err(ControlError::NotRunning) => {
            "The worker is not running; it applies this when it starts."
        }
        // An older worker that does not know the request, or one that did
        // not answer: the file is right, a restart reads it.
        _ => "The worker did not apply it yet: run `flightdeck worker restart`.",
    }
}

/// `contents` with `name` added to (`disable`) or removed from `[worker]
/// disabled_agents`, keeping every comment and every other value, and the
/// resulting list. The `[worker]` table and the key are created when absent.
pub fn set_disabled(
    contents: &str,
    name: &str,
    disable: bool,
) -> Result<(String, Vec<String>), String> {
    use toml_edit::{Array, DocumentMut, Item, Table, Value};

    let mut doc: DocumentMut = contents.parse().map_err(|e| format!("{e}"))?;
    let worker = doc
        .entry("worker")
        .or_insert(Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or("[worker] is not a table")?;
    let mut agents: Vec<String> = match worker.get("disabled_agents") {
        None => Vec::new(),
        Some(item) => item
            .as_array()
            .ok_or("worker.disabled_agents is not an array")?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or("worker.disabled_agents holds a non-string")
            })
            .collect::<Result<_, _>>()?,
    };
    agents.retain(|k| k != name);
    if disable {
        agents.push(name.to_string());
    }
    let mut array = Array::new();
    for key in &agents {
        array.push(key.as_str());
    }
    match worker.get_mut("disabled_agents") {
        // Replace the value in place, keeping the key's own decoration.
        Some(item) => {
            let decor = item.as_value().map(|v| v.decor().clone());
            let mut value = Value::Array(array);
            if let Some(decor) = decor {
                *value.decor_mut() = decor;
            }
            *item = Item::Value(value);
        }
        None => {
            worker.insert("disabled_agents", Item::Value(Value::Array(array)));
        }
    }
    Ok((doc.to_string(), agents))
}

#[cfg(test)]
mod tests;
