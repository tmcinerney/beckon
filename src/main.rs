use std::{
    collections::BTreeMap,
    env, fs,
    io::{BufRead, BufReader, ErrorKind, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
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
const CONFIG_VERSION: u32 = 1;
const STATE_VERSION: u32 = 1;

#[derive(Parser)]
#[command(about = "Bind Herdr panes to Glove80 Beckon keys")]
struct Cli {
    #[command(subcommand)]
    command: CommandLine,
}

#[derive(Subcommand)]
enum CommandLine {
    /// Create a commented configuration template without overwriting an existing file.
    Init,
    /// Validate the Beckon configuration file.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
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

#[derive(Subcommand)]
enum ConfigCommand {
    Check,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    config_version: u32,
    #[serde(default)]
    focus: FocusConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FocusConfig {
    /// Executable followed by arguments. It runs before `herdr agent focus`.
    command: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BindingState {
    state_version: u32,
    #[serde(default)]
    bindings: Vec<Binding>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Binding {
    key: String,
    pane_id: String,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        CommandLine::Init => init_config(),
        CommandLine::Config {
            command: ConfigCommand::Check,
        } => {
            load_config()?;
            println!("{} is valid", config_path().display());
            Ok(())
        }
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

fn config_dir() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(env::temp_dir)
        .join("beckon")
}

fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

fn init_config() -> Result<()> {
    let path = config_path();
    if path.exists() {
        bail!(
            "{} already exists; Beckon will not overwrite it",
            path.display()
        );
    }
    fs::create_dir_all(config_dir())
        .with_context(|| format!("create {}", config_dir().display()))?;
    fs::write(&path, DEFAULT_CONFIG).with_context(|| format!("write {}", path.display()))?;
    println!("created {}", path.display());
    Ok(())
}

fn load_config() -> Result<Config> {
    let path = config_path();
    let contents = fs::read_to_string(&path).with_context(|| {
        format!(
            "read {} (run `beckon init` to create a configuration template)",
            path.display()
        )
    })?;
    let config: Config =
        toml::from_str(&contents).with_context(|| format!("parse {}", path.display()))?;
    if config.config_version != CONFIG_VERSION {
        bail!(
            "{} has config_version {}; this Beckon version supports {}",
            path.display(),
            config.config_version,
            CONFIG_VERSION
        );
    }
    if let Some(command) = &config.focus.command
        && command.is_empty()
    {
        bail!("focus.command must contain an executable when set");
    }
    Ok(config)
}

const DEFAULT_CONFIG: &str = r#"# Beckon's portable settings. Machine-specific focus behavior is optional.
config_version = 1

# Run this before Beckon focuses a Herdr agent pane. Leave commented out when
# Ghostty is already frontmost. Use an executable and its arguments, not a shell
# string, so no shell quoting or interpolation is involved.
# [focus]
# command = ["/Users/you/.config/beckon/focus-ghostty"]
"#;

fn state_dir() -> PathBuf {
    env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(env::temp_dir)
        .join("beckon")
}

fn bindings_path() -> PathBuf {
    state_dir().join("bindings.json")
}

fn load_binding_state() -> Result<Option<BindingState>> {
    let path = bindings_path();
    if !path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let state: BindingState =
        serde_json::from_str(&contents).with_context(|| format!("parse {}", path.display()))?;
    if state.state_version != STATE_VERSION {
        bail!(
            "{} has state_version {}; this Beckon version supports {}",
            path.display(),
            state.state_version,
            STATE_VERSION
        );
    }
    validate_bindings(&state.bindings)?;
    Ok(Some(state))
}

fn save_binding_state(state: &BindingState) -> Result<()> {
    validate_bindings(&state.bindings)?;
    let directory = state_dir();
    fs::create_dir_all(&directory).with_context(|| format!("create {}", directory.display()))?;
    let path = bindings_path();
    let temporary = directory.join(format!(".bindings-{}.tmp", std::process::id()));
    let contents = serde_json::to_vec_pretty(state)?;
    fs::write(&temporary, contents).with_context(|| format!("write {}", temporary.display()))?;
    fs::rename(&temporary, &path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

fn validate_bindings(bindings: &[Binding]) -> Result<()> {
    for binding in bindings {
        valid_key(&binding.key)?;
        if binding.pane_id.is_empty() {
            bail!("a binding has an empty pane_id");
        }
    }
    for (index, binding) in bindings.iter().enumerate() {
        if bindings[index + 1..]
            .iter()
            .any(|other| other.key == binding.key || other.pane_id == binding.pane_id)
        {
            bail!("bindings must have unique keys and pane IDs");
        }
    }
    Ok(())
}

fn import_or_load_binding_state(panes: &[Pane]) -> Result<BindingState> {
    if let Some(state) = load_binding_state()? {
        return Ok(state);
    }

    // A one-time migration preserves prototype/manual fkey tokens. Later token
    // changes never become authoritative; state is durable across Herdr restarts.
    let bindings = panes
        .iter()
        .filter_map(|pane| {
            binding(pane).map(|key| Binding {
                key: key.to_string(),
                pane_id: pane.pane_id.clone(),
            })
        })
        .collect::<Vec<_>>();
    let state = BindingState {
        state_version: STATE_VERSION,
        bindings,
    };
    save_binding_state(&state)?;
    Ok(state)
}

fn reconcile_bindings(panes: &[Pane]) -> Result<BindingState> {
    let imported_tokens = load_binding_state()?.is_none();
    let mut state = import_or_load_binding_state(panes)?;
    state
        .bindings
        .retain(|binding| panes.iter().any(|pane| pane.pane_id == binding.pane_id));
    save_binding_state(&state)?;

    for pane in panes {
        match state
            .bindings
            .iter()
            .find(|binding| binding.pane_id == pane.pane_id)
        {
            Some(expected) if binding(pane) != Some(expected.key.as_str()) => {
                write_token(&pane.pane_id, TokenWrite::Set(expected.key.clone()))?;
            }
            // Only remove orphaned tokens after the one-time migration. During
            // migration, every valid fkey token was deliberately imported.
            None if !imported_tokens && binding(pane).is_some() => {
                write_token(&pane.pane_id, TokenWrite::Clear)?;
            }
            _ => {}
        }
    }
    Ok(state)
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
    let labels = register_hotkeys(&manager)?;

    eprintln!("Listening for Beckon F keys. Press Control-C to stop.");
    eprintln!("Logging presses to {}", log_path.display());
    let receiver = GlobalHotKeyEvent::receiver();
    event_loop.run(move |_event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        while let Ok(event) = receiver.try_recv() {
            if event.state != HotKeyState::Pressed {
                continue;
            }
            let Some(key) = labels.get(&event.id) else {
                continue;
            };
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before Unix epoch")
                .as_millis();
            let line = format!("{timestamp}\t{key}\t{}\n", hotkey_description(key));
            print!("{line}");
            let _ = std::io::stdout().flush();
            if let Err(error) = log.write_all(line.as_bytes()).and_then(|_| log.flush()) {
                eprintln!("write {}: {error}", log_path.display());
            }
        }
        let _keep_manager_registered = &manager;
    })
}

fn register_hotkeys(manager: &GlobalHotKeyManager) -> Result<BTreeMap<u32, &'static str>> {
    let mut labels = BTreeMap::new();
    for (key, modifiers, code) in beckon_hotkeys() {
        let hotkey = HotKey::new(modifiers, code);
        manager
            .register(hotkey)
            .with_context(|| format!("register {key}; another application may already own it"))?;
        labels.insert(hotkey.id(), key);
    }
    Ok(labels)
}

fn hotkey_description(key: &str) -> &'static str {
    match key {
        "f1" => "F16",
        "f2" => "F17",
        "f3" => "F18",
        "f4" => "F19",
        "f5" => "F20",
        "f6" => "Shift+F16",
        "f7" => "Shift+F17",
        "f8" => "Shift+F18",
        "f9" => "Shift+F19",
        "f10" => "Shift+F20",
        _ => "unknown",
    }
}

fn beckon_hotkeys() -> [(&'static str, Option<Modifiers>, Code); 10] {
    [
        ("f1", None, Code::F16),
        ("f2", None, Code::F17),
        ("f3", None, Code::F18),
        ("f4", None, Code::F19),
        ("f5", None, Code::F20),
        ("f6", Some(Modifiers::SHIFT), Code::F16),
        ("f7", Some(Modifiers::SHIFT), Code::F17),
        ("f8", Some(Modifiers::SHIFT), Code::F18),
        ("f9", Some(Modifiers::SHIFT), Code::F19),
        ("f10", Some(Modifiers::SHIFT), Code::F20),
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
    let config = load_config()?;
    let path = socket_path();
    if path.exists() {
        match UnixStream::connect(&path) {
            Ok(_) => bail!("beckond is already listening on {}", path.display()),
            Err(error) if error.kind() == ErrorKind::ConnectionRefused => {
                fs::remove_file(&path)
                    .with_context(|| format!("remove stale socket {}", path.display()))?;
            }
            Err(error) => {
                return Err(error).with_context(|| format!("connect to {}", path.display()));
            }
        }
    }
    let listener = UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
    listener
        .set_nonblocking(true)
        .with_context(|| format!("make {} nonblocking", path.display()))?;
    eprintln!("beckond listening on {}", path.display());

    // global-hotkey requires the manager and macOS event loop to share the main
    // thread. Polling the local socket here keeps `beckon bind` responsive while
    // avoiding a second daemon thread that could race state writes.
    let event_loop = EventLoopBuilder::new().build();
    let manager = GlobalHotKeyManager::new().context("initialize macOS global hotkeys")?;
    let labels = register_hotkeys(&manager)?;
    let receiver = GlobalHotKeyEvent::receiver();
    event_loop.run(move |_event, _, control_flow| {
        *control_flow =
            ControlFlow::WaitUntil(std::time::Instant::now() + Duration::from_millis(50));
        while let Ok((stream, _)) = listener.accept() {
            if let Err(error) = handle_connection(stream) {
                eprintln!("request failed: {error:#}");
            }
        }
        while let Ok(event) = receiver.try_recv() {
            if event.state != HotKeyState::Pressed {
                continue;
            }
            let Some(key) = labels.get(&event.id) else {
                continue;
            };
            if let Err(error) = focus_key(key, &config) {
                eprintln!("focus {key}: {error:#}");
            }
        }
        let _keep_manager_registered = &manager;
    })
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

fn focus_key(key: &str, config: &Config) -> Result<()> {
    let panes = panes()?;
    let state = reconcile_bindings(&panes)?;
    let binding = state
        .bindings
        .iter()
        .find(|binding| binding.key == key)
        .with_context(|| format!("{key} is not bound"))?;

    run_focus_command(config)?;
    focus_agent(&binding.pane_id)
}

fn run_focus_command(config: &Config) -> Result<()> {
    let Some(command) = config.focus.command.as_deref() else {
        return Ok(());
    };
    let (program, arguments) = command
        .split_first()
        .expect("load_config rejects an empty focus command");
    let status = Command::new(program)
        .args(arguments)
        .status()
        .with_context(|| format!("run focus command {program}"))?;
    if !status.success() {
        bail!("focus command {program} exited with {status}");
    }
    Ok(())
}

fn focus_agent(pane_id: &str) -> Result<()> {
    let output = Command::new("herdr")
        .args(["agent", "focus", pane_id])
        .output()
        .context("run herdr agent focus")?;
    if !output.status.success() {
        bail!(
            "herdr agent focus failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
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
    panes
        .iter()
        .find(|pane| pane.pane_id == pane_id)
        .context("pane no longer exists")?;
    let mut state = reconcile_bindings(&panes)?;
    let key = match requested_key {
        Some(key) => valid_key(key)?.to_string(),
        None => first_free_key(&state.bindings)
            .context("no Beckon keys are free")?
            .to_string(),
    };

    if let Some(owner) = state.bindings.iter().find(|candidate| candidate.key == key)
        && owner.pane_id != pane_id
    {
        bail!("{key} is already bound to {}", owner.pane_id);
    }
    if state
        .bindings
        .iter()
        .any(|binding| binding.pane_id == pane_id && binding.key == key)
    {
        return Ok(json!({"pane_id": pane_id, "key": key, "changed": false}));
    }
    state.bindings.retain(|binding| binding.pane_id != pane_id);
    state.bindings.push(Binding {
        key: key.clone(),
        pane_id: pane_id.to_string(),
    });
    save_binding_state(&state)?;
    write_token(pane_id, TokenWrite::Set(key.clone()))?;
    Ok(json!({"pane_id": pane_id, "key": key, "changed": true}))
}

fn release(pane_id: &str) -> Result<Value> {
    let panes = panes()?;
    panes
        .iter()
        .find(|pane| pane.pane_id == pane_id)
        .context("pane no longer exists")?;
    let mut state = reconcile_bindings(&panes)?;
    let before = state.bindings.len();
    state.bindings.retain(|binding| binding.pane_id != pane_id);
    if state.bindings.len() == before {
        return Ok(json!({"pane_id": pane_id, "changed": false}));
    }
    save_binding_state(&state)?;
    write_token(pane_id, TokenWrite::Clear)?;
    Ok(json!({"pane_id": pane_id, "changed": true}))
}

fn status() -> Result<Value> {
    let panes = panes()?;
    let state = reconcile_bindings(&panes)?;
    let mut bindings: Vec<_> = state
        .bindings
        .into_iter()
        .filter_map(|binding| {
            panes
                .iter()
                .find(|pane| pane.pane_id == binding.pane_id)
                .map(|pane| json!({"key": binding.key, "pane": pane}))
        })
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
fn valid_key(key: &str) -> Result<&str> {
    KEY_IDS
        .iter()
        .copied()
        .find(|candidate| *candidate == key)
        .context("key must be f1 through f10")
}

fn first_free_key(bindings: &[Binding]) -> Option<&'static str> {
    KEY_IDS
        .into_iter()
        .find(|key| !bindings.iter().any(|binding| binding.key == *key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chooses_the_first_unbound_key() {
        let binding = Binding {
            key: "f2".into(),
            pane_id: "p1".into(),
        };
        assert_eq!(first_free_key(&[binding]), Some("f1"));
    }

    #[test]
    fn rejects_duplicate_binding_keys() {
        let bindings = vec![
            Binding {
                key: "f1".into(),
                pane_id: "p1".into(),
            },
            Binding {
                key: "f1".into(),
                pane_id: "p2".into(),
            },
        ];
        assert!(validate_bindings(&bindings).is_err());
    }
}
