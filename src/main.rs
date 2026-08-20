use std::{
    collections::BTreeMap,
    env, fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tao::event_loop::{ControlFlow, EventLoopBuilder};

const SOURCE: &str = "beckond";
const KEY_IDS: [&str; 10] = ["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10"];

#[derive(Parser)]
#[command(about = "Bind Herdr panes to Glove80 Beckon keys")]
struct Cli {
    #[command(subcommand)]
    command: CommandLine,
}

#[derive(Subcommand)]
enum CommandLine {
    /// Run the local single-writer binding daemon.
    Daemon,
    /// Bind a pane to a Beckon key. Defaults to $HERDR_PANE_ID.
    Bind(BindArgs),
    /// Clear a pane's Beckon binding. Defaults to $HERDR_PANE_ID.
    Release(PaneArgs),
    /// Print every current Beckon binding as JSON.
    Status,
    /// Log the ten Beckon-layer function-key events until interrupted.
    ListenKeys,
}

#[derive(Args)]
struct BindArgs {
    #[arg(long)]
    key: Option<String>,
    #[command(flatten)]
    pane: PaneArgs,
}

#[derive(Args)]
struct PaneArgs {
    #[arg(long)]
    pane: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Bind {
        pane_id: String,
        key: Option<String>,
    },
    Release {
        pane_id: String,
    },
    Status,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Response {
    Ok { data: Value },
    Error { message: String },
}

#[derive(Debug, Deserialize)]
struct PaneListResponse {
    result: PaneListResult,
}

#[derive(Debug, Deserialize)]
struct PaneListResult {
    panes: Vec<Pane>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct Pane {
    pane_id: String,
    agent_status: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    tokens: Option<std::collections::BTreeMap<String, String>>,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        CommandLine::Daemon => daemon(),
        CommandLine::Bind(args) => client(Request::Bind {
            pane_id: current_pane(args.pane.pane)?,
            key: args.key,
        }),
        CommandLine::Release(args) => client(Request::Release {
            pane_id: current_pane(args.pane)?,
        }),
        CommandLine::Status => client(Request::Status),
        CommandLine::ListenKeys => listen_keys(),
    }
}

fn state_dir() -> PathBuf {
    env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(env::temp_dir)
        .join("beckon")
}

fn listen_keys() -> Result<()> {
    let log_dir = state_dir();
    fs::create_dir_all(&log_dir).with_context(|| format!("create {}", log_dir.display()))?;
    let log_path = log_dir.join("key-events.log");
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open {}", log_path.display()))?;

    // macOS requires this event loop and the hotkey manager on its main thread.
    let event_loop = EventLoopBuilder::new().build();
    let manager = GlobalHotKeyManager::new().context("initialize macOS global hotkeys")?;
    let mut labels = BTreeMap::new();
    for (label, modifiers, code) in beckon_hotkeys() {
        let hotkey = HotKey::new(modifiers, code);
        manager
            .register(hotkey)
            .with_context(|| format!("register {label}; another application may already own it"))?;
        labels.insert(hotkey.id(), label);
    }

    eprintln!("Listening for Beckon F keys. Press Control-C to stop.");
    eprintln!("Logging presses to {}", log_path.display());
    let receiver = GlobalHotKeyEvent::receiver();
    event_loop.run(move |_event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        while let Ok(event) = receiver.try_recv() {
            if event.state != HotKeyState::Pressed {
                continue;
            }
            let Some(label) = labels.get(&event.id) else {
                continue;
            };
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before Unix epoch")
                .as_millis();
            let line = format!("{timestamp}\t{label}\n");
            print!("{line}");
            let _ = std::io::stdout().flush();
            if let Err(error) = log.write_all(line.as_bytes()).and_then(|_| log.flush()) {
                eprintln!("write {}: {error}", log_path.display());
            }
        }
        let _keep_manager_registered = &manager;
    })
}

fn beckon_hotkeys() -> [(&'static str, Option<Modifiers>, Code); 10] {
    [
        ("f1\tF16", None, Code::F16),
        ("f2\tF17", None, Code::F17),
        ("f3\tF18", None, Code::F18),
        ("f4\tF19", None, Code::F19),
        ("f5\tF20", None, Code::F20),
        ("f6\tShift+F16", Some(Modifiers::SHIFT), Code::F16),
        ("f7\tShift+F17", Some(Modifiers::SHIFT), Code::F17),
        ("f8\tShift+F18", Some(Modifiers::SHIFT), Code::F18),
        ("f9\tShift+F19", Some(Modifiers::SHIFT), Code::F19),
        ("f10\tShift+F20", Some(Modifiers::SHIFT), Code::F20),
    ]
}

fn current_pane(explicit: Option<String>) -> Result<String> {
    explicit
        .or_else(|| env::var("HERDR_PANE_ID").ok())
        .context("no pane supplied; use --pane <PANE_ID> or run inside a Herdr pane")
}

fn socket_path() -> PathBuf {
    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("beckon.sock")
}

fn daemon() -> Result<()> {
    let path = socket_path();
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("remove stale socket {}", path.display()))?;
    }
    let listener = UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
    eprintln!("beckond listening on {}", path.display());

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(error) = handle_connection(stream) {
                    eprintln!("request failed: {error:#}");
                }
            }
            Err(error) => eprintln!("accept failed: {error}"),
        }
    }
    Ok(())
}

fn handle_connection(mut stream: UnixStream) -> Result<()> {
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    let response = match serde_json::from_str::<Request>(&line) {
        Ok(request) => match dispatch(request) {
            Ok(data) => Response::Ok { data },
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Err(error) => Response::Error {
            message: format!("invalid request: {error}"),
        },
    };
    writeln!(stream, "{}", serde_json::to_string(&response)?)?;
    Ok(())
}

fn client(request: Request) -> Result<()> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path).with_context(|| {
        format!(
            "connect to {} (start `beckon daemon` first)",
            path.display()
        )
    })?;
    writeln!(stream, "{}", serde_json::to_string(&request)?)?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    match serde_json::from_str::<Response>(&line)? {
        Response::Ok { data } => {
            println!("{}", serde_json::to_string_pretty(&data)?);
            Ok(())
        }
        Response::Error { message } => bail!(message),
    }
}

fn dispatch(request: Request) -> Result<Value> {
    match request {
        Request::Bind { pane_id, key } => bind(&pane_id, key.as_deref()),
        Request::Release { pane_id } => release(&pane_id),
        Request::Status => status(),
    }
}

fn panes() -> Result<Vec<Pane>> {
    let output = Command::new("herdr")
        .args(["pane", "list"])
        .output()
        .context("run herdr pane list")?;
    if !output.status.success() {
        bail!(
            "herdr pane list failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(serde_json::from_slice::<PaneListResponse>(&output.stdout)?
        .result
        .panes)
}

fn binding(pane: &Pane) -> Option<&str> {
    pane.tokens.as_ref()?.get("fkey").map(String::as_str)
}

fn bind(pane_id: &str, requested_key: Option<&str>) -> Result<Value> {
    let panes = panes()?;
    let pane = panes
        .iter()
        .find(|pane| pane.pane_id == pane_id)
        .context("pane no longer exists")?;
    let key = match requested_key {
        Some(key) => valid_key(key)?.to_string(),
        None => first_free_key(&panes)
            .context("no Beckon keys are free")?
            .to_string(),
    };

    if let Some(owner) = panes
        .iter()
        .find(|candidate| binding(candidate) == Some(key.as_str()))
    {
        if owner.pane_id != pane_id {
            bail!("{key} is already bound to {}", owner.pane_id);
        }
    }
    if binding(pane) == Some(key.as_str()) {
        return Ok(json!({"pane_id": pane_id, "key": key, "changed": false}));
    }
    if binding(pane).is_some() {
        write_token(pane_id, false)?;
    }
    write_token(pane_id, true_with_key(&key))?;
    Ok(json!({"pane_id": pane_id, "key": key, "changed": true}))
}

fn release(pane_id: &str) -> Result<Value> {
    let pane = panes()?
        .into_iter()
        .find(|pane| pane.pane_id == pane_id)
        .context("pane no longer exists")?;
    if binding(&pane).is_none() {
        return Ok(json!({"pane_id": pane_id, "changed": false}));
    }
    write_token(pane_id, false)?;
    Ok(json!({"pane_id": pane_id, "changed": true}))
}

fn status() -> Result<Value> {
    let mut bindings: Vec<_> = panes()?
        .into_iter()
        .filter_map(|pane| binding(&pane).map(|key| json!({"key": key, "pane": pane})))
        .collect();
    bindings.sort_by(|left, right| left["key"].as_str().cmp(&right["key"].as_str()));
    Ok(json!({"bindings": bindings}))
}

fn write_token(pane_id: &str, set: impl Into<TokenWrite>) -> Result<()> {
    let mut command = Command::new("herdr");
    command.args(["pane", "report-metadata", pane_id, "--source", SOURCE]);
    match set.into() {
        TokenWrite::Set(key) => command.arg("--token").arg(format!("fkey={key}")),
        TokenWrite::Clear => command.arg("--clear-token").arg("fkey"),
    };
    let output = command.output().context("write Herdr pane token")?;
    if !output.status.success() {
        bail!(
            "token update failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

enum TokenWrite {
    Set(String),
    Clear,
}
impl From<bool> for TokenWrite {
    fn from(value: bool) -> Self {
        assert!(!value);
        Self::Clear
    }
}
fn true_with_key(key: &str) -> TokenWrite {
    TokenWrite::Set(key.to_string())
}

fn valid_key(key: &str) -> Result<&str> {
    KEY_IDS
        .iter()
        .copied()
        .find(|candidate| *candidate == key)
        .context("key must be f1 through f10")
}

fn first_free_key(panes: &[Pane]) -> Option<&'static str> {
    KEY_IDS
        .into_iter()
        .find(|key| !panes.iter().any(|pane| binding(pane) == Some(*key)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chooses_the_first_unbound_key() {
        let pane = Pane {
            pane_id: "p1".into(),
            agent_status: "idle".into(),
            agent: None,
            label: None,
            cwd: None,
            tokens: None,
        };
        assert_eq!(first_free_key(&[pane]), Some("f1"));
    }
}
