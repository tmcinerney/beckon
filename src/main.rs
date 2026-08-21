use std::{
    collections::BTreeMap,
    env,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, ErrorKind, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use beckon::{
    config,
    core::{BindingService, PaneDirectory},
    focus::{CommandFocus, FocusAdapter},
    herdr::{HerdrCli, LivePaneDirectory},
    state::JsonBindingStore,
};
use clap::{Args, Parser, Subcommand};
use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tao::event_loop::{ControlFlow, EventLoopBuilder};

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
    /// Print the hardware-neutral LED plan. This does not write to a keyboard.
    Preview(PreviewArgs),
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

#[derive(Args)]
struct PreviewArgs {
    /// Show one example for every agent state without querying Herdr.
    #[arg(long)]
    all_states: bool,
    /// Emit the render plan as JSON.
    #[arg(long)]
    json: bool,
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

fn main() -> Result<()> {
    match Cli::parse().command {
        CommandLine::Init => config::initialize(),
        CommandLine::Config {
            command: ConfigCommand::Check,
        } => {
            config::load()?;
            println!("{} is valid", config::path().display());
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
        CommandLine::Preview(args) => preview(args),
        CommandLine::ListenKeys => listen_keys(),
    }
}

fn preview(args: PreviewArgs) -> Result<()> {
    let config = config::load()?;
    let plan = if args.all_states {
        beckon::render::all_state_examples(&config.display)?
    } else {
        let store = JsonBindingStore::from_environment();
        let herdr = HerdrCli;
        let bindings = BindingService::new(&store, &herdr);
        beckon::render::render(&config.display, &bindings.status()?)?
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
    } else {
        println!("Preview only: no keyboard HID frames are written.");
        for key in plan.keys {
            let state = key.state.map_or("unbound".to_string(), |state| {
                format!("{state:?}").to_lowercase()
            });
            println!(
                "{}\t{state}\t{}\t{:.1}\t{:?}",
                key.key, key.colour, key.brightness, key.motion
            );
        }
    }
    Ok(())
}

fn listen_keys() -> Result<()> {
    let event_loop = EventLoopBuilder::new().build();
    let manager = GlobalHotKeyManager::new().context("initialize macOS global hotkeys")?;
    let labels = register_hotkeys(&manager)?;
    eprintln!("Listening for Beckon F keys. Press Control-C to stop.");
    eprintln!("Logging presses to {}", key_event_log_path().display());
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
            let line = key_event_line(key, "listener");
            print!("{line}");
            let _ = std::io::stdout().flush();
            if let Err(error) = append_key_event(&line) {
                eprintln!("record key event: {error:#}");
            }
        }
        let _keep_manager_registered = &manager;
    })
}

fn key_event_log_path() -> PathBuf {
    JsonBindingStore::from_environment()
        .directory()
        .join("key-events.log")
}

fn key_event_line(key: &str, source: &str) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before Unix epoch")
        .as_millis();
    format!(
        "{timestamp}\t{key}\t{}\t{source}\n",
        hotkey_description(key)
    )
}

fn focus_result_line(key: &str, result: &str) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before Unix epoch")
        .as_millis();
    format!("{timestamp}\t{key}\tfocus-{result}\tdaemon\n")
}

fn append_key_event(line: &str) -> Result<()> {
    let path = key_event_log_path();
    let directory = path.parent().expect("key event log has a parent");
    fs::create_dir_all(directory).with_context(|| format!("create {}", directory.display()))?;
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    log.write_all(line.as_bytes())
        .and_then(|_| log.flush())
        .with_context(|| format!("write {}", path.display()))
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
        .map(|directory| directory.join("beckon.sock"))
        // A launchd service and an interactive shell can have distinct TMPDIRs.
        // State is the stable local fallback for this single-user daemon.
        .unwrap_or_else(|| {
            JsonBindingStore::from_environment()
                .directory()
                .join("beckon.sock")
        })
}

fn daemon() -> Result<()> {
    let config = config::load()?;
    let path = socket_path();
    if let Some(directory) = path.parent()
        && directory == JsonBindingStore::from_environment().directory()
    {
        JsonBindingStore::from_environment().ensure_directory()?;
    }
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

    // AIDEV-NOTE: macOS requires global-hotkey and Tao's event loop on the main
    // thread. Socket polling keeps binding mutation serialized in this daemon.
    let herdr = LivePaneDirectory::start()?;
    let event_loop = EventLoopBuilder::new().build();
    let manager = GlobalHotKeyManager::new().context("initialize macOS global hotkeys")?;
    let labels = register_hotkeys(&manager)?;
    let receiver = GlobalHotKeyEvent::receiver();
    event_loop.run(move |_event, _, control_flow| {
        *control_flow =
            ControlFlow::WaitUntil(std::time::Instant::now() + Duration::from_millis(50));
        while let Ok((stream, _)) = listener.accept() {
            if let Err(error) = handle_connection(stream, &herdr) {
                eprintln!("request failed: {error:#}");
            }
        }
        while let Ok(event) = receiver.try_recv() {
            if event.state == HotKeyState::Pressed
                && let Some(key) = labels.get(&event.id)
            {
                if let Err(error) = append_key_event(&key_event_line(key, "daemon")) {
                    eprintln!("record {key}: {error:#}");
                }
                match focus_key(key, &config, &herdr) {
                    Ok(()) => {
                        if let Err(error) = append_key_event(&focus_result_line(key, "ok")) {
                            eprintln!("record focus {key}: {error:#}");
                        }
                    }
                    Err(error) => {
                        if let Err(record_error) =
                            append_key_event(&focus_result_line(key, "error"))
                        {
                            eprintln!("record focus {key}: {record_error:#}");
                        }
                        eprintln!("focus {key}: {error:#}");
                    }
                }
            }
        }
        let _keep_manager_registered = &manager;
    })
}

fn handle_connection<D: PaneDirectory>(mut stream: UnixStream, panes: &D) -> Result<()> {
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    let response = match serde_json::from_str::<Request>(&line) {
        Ok(request) => match dispatch(request, panes) {
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

fn dispatch<D: PaneDirectory>(request: Request, panes: &D) -> Result<Value> {
    let store = JsonBindingStore::from_environment();
    let bindings = BindingService::new(&store, panes);
    match request {
        Request::Bind { pane_id, key } => Ok(serde_json::to_value(
            bindings.bind(&pane_id, key.as_deref())?,
        )?),
        Request::Release { pane_id } => {
            Ok(json!({"pane_id": pane_id, "changed": bindings.release(&pane_id)?}))
        }
        Request::Status => Ok(json!({
            "bindings": bindings.status()?.into_iter().map(|(binding, pane)| json!({
                "key": binding.key,
                "pane": pane,
            })).collect::<Vec<_>>()
        })),
    }
}

fn focus_key<D: PaneDirectory>(key: &str, config: &config::Config, panes: &D) -> Result<()> {
    let store = JsonBindingStore::from_environment();
    let bindings = BindingService::new(&store, panes);
    let pane_id = bindings.pane_for_key(key)?;
    CommandFocus::new(&config.focus).focus_terminal()?;
    panes.focus_agent(&pane_id)
}
