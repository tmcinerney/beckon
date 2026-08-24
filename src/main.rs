use std::{
    collections::BTreeMap,
    env,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, ErrorKind, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use beckon::{
    action::RepeatPressConfirm,
    config::{self, InputProfile},
    core::{
        BindingService, BindingState, BindingStore, PaneDirectory, PanePresentation,
        PresentationTokenWrite, STATE_VERSION,
    },
    focus::{CommandFocus, FocusAdapter},
    herdr::{HerdrCli, LivePaneDirectory},
    hid::{self, Status, StatusSnapshot},
    input::{
        Glove80HotkeyInput, InputAdapter, MacbookFunctionKeyInput, RegisteredInput,
        register_adapters,
    },
    state::JsonBindingStore,
};
use clap::{Args, Parser, Subcommand};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tao::event_loop::{ControlFlow, EventLoopBuilder};

#[derive(Parser)]
#[command(
    name = "beckon",
    version,
    about = "Bind selected Herdr panes to Beckon navigation keys",
    long_about = "Beckon is a local, display-and-navigation-first companion for Herdr.\n\
It can bind an explicitly selected pane to an F key, show bindings, and focus a\n\
bound pane. It never sends agent input, approves tools, or answers prompts unless\n\
the optional repeat-press confirmation action is explicitly enabled.",
    after_help = "AGENT WORKFLOW:\n\
  1. Run `beckon status` to inspect currently occupied keys.\n\
  2. In the intended Herdr pane, run `beckon bind --key f3`; omit --key only\n\
     when first-free assignment is intended.\n\
  3. From any context, run `beckon release --key f3` to clear that key.\n\
\n\
The `beckond` command is an installed PATH wrapper for `beckon daemon`. Normally\n\
Home Manager starts it. Start it manually only for local development or recovery.\n\
Use `beckon hid` only for wired firmware diagnostics; its write commands affect\n\
LED status display, never ordinary keyboard input."
)]
struct Cli {
    #[command(subcommand)]
    command: CommandLine,
}

#[derive(Subcommand)]
enum CommandLine {
    /// Create a commented configuration template without overwriting an existing file.
    #[command(
        long_about = "Create the optional machine-local configuration template.\n\
This is safe to run repeatedly: it refuses to overwrite an existing file."
    )]
    Init,
    /// Validate the Beckon configuration file.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Run the local single-writer binding daemon (normally invoked as beckond).
    #[command(
        long_about = "Run Beckon's local daemon. It serializes binding changes, follows Herdr\n\
pane state, renders LEDs, and handles global key navigation.\n\
\n\
Normally Home Manager runs this as `beckond`; do not start a second copy unless\n\
you are deliberately recovering or developing the service."
    )]
    Daemon,
    /// Explicitly bind a selected pane to a Beckon key. Defaults to $HERDR_PANE_ID.
    #[command(
        long_about = "Explicitly register one Herdr pane with one physical Beckon key.\n\
\n\
Run this in the pane being registered, or provide `--pane <pane-id>`. Specify\n\
`--key f1` through `--key f10` to choose a key. Omitting --key deliberately uses\n\
the first available key, except that an already-bound pane keeps its existing\n\
key. Beckon never auto-registers panes or agents."
    )]
    Bind(BindArgs),
    /// Clear a pane's Beckon binding. Defaults to $HERDR_PANE_ID; --key works from any pane.
    #[command(
        long_about = "Remove a Beckon registration. Run this in the bound pane, provide\n\
`--pane <pane-id>`, or use `--key f1` through `--key f10` from anywhere.\n\
This changes only the local Beckon binding and the pane's visible fkey token; it\n\
does not close a pane or control an agent."
    )]
    Release(ReleaseArgs),
    /// Print every live Herdr pane with its resolved title and Beckon binding.
    Status,
    /// Print the hardware-neutral LED plan. This does not write to a keyboard.
    Preview(PreviewArgs),
    /// Inspect or explicitly test the USB-only Beckon status endpoint.
    #[command(
        long_about = "Inspect or test the wired, vendor-specific Glove80 status endpoint.\n\
`list` and `probe` are read-only. `send` changes only status LEDs.\n\
`send-malformed --confirm` intentionally sends a rejected test frame. None of\n\
these commands write ordinary keyboard input or control Herdr agents."
    )]
    Hid {
        #[command(subcommand)]
        command: HidCommand,
    },
    /// Log the ten Beckon-layer function-key events until interrupted.
    ListenKeys,
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Parse the configuration without starting the daemon or changing state.
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
struct ReleaseArgs {
    /// Clear the binding assigned to this physical key from any pane.
    #[arg(long)]
    key: Option<String>,
    #[command(flatten)]
    pane: PaneArgs,
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

#[derive(Subcommand)]
enum HidCommand {
    /// List matching USB vendor HID interfaces without opening or writing them.
    List,
    /// Open the one matching USB vendor interface without writing keyboard state.
    Probe,
    /// Send one caller-supplied, valid 32-byte status snapshot.
    Send(HidSendArgs),
    /// Send a malformed short report to verify firmware rejection.
    SendMalformed(HidMalformedArgs),
}

#[derive(Args)]
struct HidSendArgs {
    /// Snapshot sequence number (0 through 255).
    #[arg(long)]
    sequence: u8,
    /// Exactly ten comma-separated states, F1 through F10.
    #[arg(long, value_delimiter = ',', num_args = 1..)]
    states: Vec<Status>,
}

#[derive(Args)]
struct HidMalformedArgs {
    /// Required acknowledgement because this deliberately sends an invalid report.
    #[arg(long)]
    confirm: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Bind {
        pane_id: String,
        key: Option<String>,
    },
    ReleasePane {
        pane_id: String,
    },
    ReleaseKey {
        key: String,
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
        CommandLine::Release(args) => match args.key {
            Some(key) => {
                if args.pane.pane.is_some() {
                    bail!("--key and --pane cannot be used together");
                }
                client(Request::ReleaseKey { key })
            }
            None => client(Request::ReleasePane {
                pane_id: current_pane(args.pane.pane)?,
            }),
        },
        CommandLine::Status => client(Request::Status),
        CommandLine::Preview(args) => preview(args),
        CommandLine::Hid { command } => hid_command(command),
        CommandLine::ListenKeys => listen_keys(),
    }
}

fn hid_command(command: HidCommand) -> Result<()> {
    match command {
        HidCommand::List => {
            let endpoints = hid::list()?;
            if endpoints.is_empty() {
                println!(
                    "No Beckon USB status endpoints found (expected {:04X}:{:04X}, usage page 0x{:04X}, usage 0x{:04X}).",
                    hid::GLOVE80_VENDOR_ID,
                    hid::GLOVE80_PRODUCT_ID,
                    hid::VENDOR_USAGE_PAGE,
                    hid::STATUS_USAGE
                );
            } else {
                for endpoint in endpoints {
                    println!(
                        "{:04X}:{:04X}\tinterface={}\t{}",
                        endpoint.vendor_id,
                        endpoint.product_id,
                        endpoint.interface_number,
                        endpoint.path
                    );
                }
            }
            Ok(())
        }
        HidCommand::Probe => {
            let endpoint = hid::probe()?;
            println!(
                "Opened Beckon USB status endpoint {:04X}:{:04X} interface {} at {}",
                endpoint.vendor_id, endpoint.product_id, endpoint.interface_number, endpoint.path
            );
            Ok(())
        }
        HidCommand::Send(args) => {
            let slots: [Status; hid::SLOT_COUNT] =
                args.states.try_into().map_err(|states: Vec<_>| {
                    anyhow::anyhow!(
                        "expected {} states for F1 through F10, received {}",
                        hid::SLOT_COUNT,
                        states.len()
                    )
                })?;
            hid::send(StatusSnapshot::for_manual_send(args.sequence, slots))?;
            println!(
                "Sent valid Beckon status snapshot sequence {}.",
                args.sequence
            );
            Ok(())
        }
        HidCommand::SendMalformed(args) => {
            if !args.confirm {
                bail!("refusing to send a malformed report without --confirm");
            }
            hid::send_malformed()?;
            println!("Sent deliberate malformed Beckon status report.");
            Ok(())
        }
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
                key.key, key.color, key.brightness, key.motion
            );
        }
    }
    Ok(())
}

fn listen_keys() -> Result<()> {
    // Keep this diagnostic usable before `beckon init`; the daemon itself
    // requires a complete configuration for focus and display.
    let input_profiles = if config::path().exists() {
        config::load()?.input.enabled_profiles()?
    } else {
        vec![InputProfile::default()]
    };
    let event_loop = EventLoopBuilder::new().build();
    let manager = GlobalHotKeyManager::new().context("initialize macOS global hotkeys")?;
    let input = register_input(&input_profiles, &manager)?;
    eprintln!("beckon inputs: {}", input_diagnostic(&input_profiles));
    eprintln!("Listening for Beckon F keys. Press Control-C to stop.");
    eprintln!("Logging presses to {}", key_event_log_path().display());
    let receiver = GlobalHotKeyEvent::receiver();
    event_loop.run(move |_event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        while let Ok(event) = receiver.try_recv() {
            let Some(binding) = input.pressed(&event) else {
                continue;
            };
            let line = key_event_line(binding.key, binding.description, "listener");
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

fn key_event_line(key: &str, description: &str, source: &str) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before Unix epoch")
        .as_millis();
    format!("{timestamp}\t{key}\t{}\t{source}\n", description)
}

fn register_input(
    profiles: &[InputProfile],
    manager: &GlobalHotKeyManager,
) -> Result<RegisteredInput> {
    let glove80 = Glove80HotkeyInput;
    let macbook = MacbookFunctionKeyInput;
    let adapters = profiles
        .iter()
        .map(|profile| match profile {
            InputProfile::Glove80 => &glove80 as &dyn InputAdapter,
            InputProfile::MacbookFunctionKeys => &macbook as &dyn InputAdapter,
        })
        .collect::<Vec<_>>();
    register_adapters(&adapters, manager)
}

fn input_diagnostic(profiles: &[InputProfile]) -> String {
    profiles
        .iter()
        .map(|profile| profile.name())
        .collect::<Vec<_>>()
        .join(", ")
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
    let mut display = hid::RenderSink::new(hid::UsbStatusWriter);
    let mut last_display_error = None;
    let mut presentation = PresentationPublisher::default();
    let mut last_presentation_error = None;
    let event_loop = EventLoopBuilder::new().build();
    let manager = GlobalHotKeyManager::new().context("initialize macOS global hotkeys")?;
    let input_profiles = config.input.enabled_profiles()?;
    let input = register_input(&input_profiles, &manager)?;
    eprintln!("beckond inputs: {}", input_diagnostic(&input_profiles));
    let receiver = GlobalHotKeyEvent::receiver();
    let mut confirm = RepeatPressConfirm::default();
    event_loop.run(move |_event, _, control_flow| {
        *control_flow =
            ControlFlow::WaitUntil(std::time::Instant::now() + Duration::from_millis(50));
        while let Ok((stream, _)) = listener.accept() {
            if let Err(error) = handle_connection(stream, &herdr) {
                eprintln!("request failed: {error:#}");
            }
        }
        match publish_display(&config, &herdr, &mut display) {
            Ok(_) => last_display_error = None,
            Err(error) => {
                let error = format!("{error:#}");
                if last_display_error.as_deref() != Some(error.as_str()) {
                    eprintln!("update keyboard status display: {error}");
                    last_display_error = Some(error);
                }
            }
        }
        match presentation.sync(&herdr) {
            Ok(_) => last_presentation_error = None,
            Err(error) => {
                let error = format!("{error:#}");
                if last_presentation_error.as_deref() != Some(error.as_str()) {
                    eprintln!("update Herdr pane presentation: {error}");
                    last_presentation_error = Some(error);
                }
            }
        }
        while let Ok(event) = receiver.try_recv() {
            if let Some(binding) = input.pressed(&event) {
                let key = binding.key;
                if let Err(error) =
                    append_key_event(&key_event_line(key, binding.description, "daemon"))
                {
                    eprintln!("record {key}: {error:#}");
                }
                let now = Instant::now();
                let target = pane_for_key(key, &herdr);
                let confirmed = config.actions.confirm.enabled
                    && target.as_ref().is_ok_and(|pane_id| {
                        confirm.take_if_ready(key, pane_id, pane_is_focused(&herdr, pane_id), now)
                    });
                if confirmed {
                    let keys = config
                        .actions
                        .confirm
                        .keys
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>();
                    let result = target.and_then(|pane_id| herdr.send_keys(&pane_id, &keys));
                    match result {
                        Ok(()) => {
                            if let Err(error) =
                                append_key_event(&focus_result_line(key, "confirm-ok"))
                            {
                                eprintln!("record confirmation {key}: {error:#}");
                            }
                        }
                        Err(error) => {
                            if let Err(record_error) =
                                append_key_event(&focus_result_line(key, "confirm-error"))
                            {
                                eprintln!("record confirmation {key}: {record_error:#}");
                            }
                            eprintln!("confirm {key}: {error:#}");
                        }
                    }
                } else {
                    match focus_key(key, &config, &herdr) {
                        Ok(()) => {
                            if config.actions.confirm.enabled
                                && let Ok(pane_id) = target
                            {
                                confirm.arm(
                                    key,
                                    &pane_id,
                                    Duration::from_millis(config.actions.confirm.repeat_press_ms),
                                    now,
                                );
                            }
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
        }
        let _keep_manager_registered = &manager;
    })
}

/// Publishes Beckon-owned sidebar tokens only when a pane appears or its
/// binding changes. Pane IDs are immutable, so including them in the one
/// update keeps the sidebar independent of missing-token fallbacks.
#[derive(Default)]
struct PresentationPublisher {
    published_bindings: BTreeMap<String, String>,
}

impl PresentationPublisher {
    fn sync<D: PaneDirectory>(&mut self, panes: &D) -> Result<bool> {
        let store = JsonBindingStore::from_environment();
        let presentation = BindingService::new(&store, panes).panes()?;
        self.publish(panes, presentation)
    }

    fn publish<D: PaneDirectory>(
        &mut self,
        panes: &D,
        presentation: Vec<PanePresentation>,
    ) -> Result<bool> {
        let mut changed = false;
        let live = presentation
            .iter()
            .map(|pane| pane.pane_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        self.published_bindings
            .retain(|pane_id, _| live.contains(pane_id.as_str()));
        for pane in presentation {
            if self.published_bindings.get(&pane.pane_id) == Some(&pane.binding) {
                continue;
            }
            match panes.write_presentation_tokens(&pane.pane_id, &pane.binding)? {
                PresentationTokenWrite::Written => {
                    self.published_bindings.insert(pane.pane_id, pane.binding);
                    changed = true;
                }
                PresentationTokenWrite::PaneGone => {
                    // A cache snapshot may briefly outlive a pane. Remember the
                    // definitive server response to avoid retrying on every
                    // event-loop tick; `retain` removes it after reconciliation.
                    self.published_bindings.insert(pane.pane_id, pane.binding);
                }
            }
        }
        Ok(changed)
    }
}

/// Read the durable binding ledger without mutating it, combine it with the
/// live pane cache, and send a new transport snapshot only when it matters.
/// Binding reconciliation remains a CLI/request operation; polling it here
/// would rewrite the state file and Herdr metadata on every event-loop tick.
fn publish_display<D, W>(
    config: &config::Config,
    panes: &D,
    display: &mut hid::RenderSink<W>,
) -> Result<bool>
where
    D: PaneDirectory,
    W: hid::StatusWriter,
{
    let store = JsonBindingStore::from_environment();
    let state = store.load()?.unwrap_or(BindingState {
        state_version: STATE_VERSION,
        bindings: Vec::new(),
    });
    let panes_by_id = panes.panes()?;
    let bindings = state
        .bindings
        .into_iter()
        .filter_map(|binding| {
            panes_by_id
                .iter()
                .find(|pane| pane.pane_id == binding.pane_id)
                .cloned()
                .map(|pane| (binding, pane))
        })
        .collect::<Vec<_>>();
    let plan = beckon::render::render(&config.display, &bindings)?;
    display.publish(&plan)
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
        Request::ReleasePane { pane_id } => {
            Ok(json!({"pane_id": pane_id, "changed": bindings.release(&pane_id)?}))
        }
        Request::ReleaseKey { key } => {
            let pane_id = bindings.release_key(&key)?;
            Ok(json!({"key": key, "pane_id": pane_id, "changed": pane_id.is_some()}))
        }
        Request::Status => Ok(json!({
            "bindings": bindings.status()?.into_iter().map(|(binding, pane)| json!({
                "key": binding.key,
                "pane": pane,
            })).collect::<Vec<_>>(),
            "panes": bindings.panes()?,
        })),
    }
}

fn focus_key<D: PaneDirectory>(key: &str, config: &config::Config, panes: &D) -> Result<()> {
    let pane_id = pane_for_key(key, panes)?;
    CommandFocus::new(&config.focus).focus_terminal()?;
    panes.focus_pane(&pane_id)
}

fn pane_for_key<D: PaneDirectory>(key: &str, panes: &D) -> Result<String> {
    let store = JsonBindingStore::from_environment();
    BindingService::new(&store, panes).pane_for_key(key)
}

fn pane_is_focused<D: PaneDirectory>(panes: &D, pane_id: &str) -> bool {
    panes
        .panes()
        .map(|panes| {
            panes
                .into_iter()
                .any(|pane| pane.pane_id == pane_id && pane.focused)
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use clap::CommandFactory;

    fn help_for(args: &[&str]) -> String {
        let mut command = Cli::command();
        let command = args.iter().fold(&mut command, |command, name| {
            command.find_subcommand_mut(name).unwrap()
        });
        let mut output = Vec::new();
        command.write_long_help(&mut output).unwrap();
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn root_help_explains_agent_workflow_and_safety_boundary() {
        let help = help_for(&[]);
        assert!(help.contains("AGENT WORKFLOW:"));
        assert!(help.contains("optional repeat-press confirmation action is explicitly enabled"));
        assert!(help.contains("beckond"));
    }

    #[test]
    fn binding_and_release_help_explain_explicit_registration() {
        let bind_help = help_for(&["bind"]);
        assert!(bind_help.contains("Beckon never auto-registers panes or agents"));
        assert!(bind_help.contains("--key f1"));

        let release_help = help_for(&["release"]);
        assert!(release_help.contains("does not close a pane or control an agent"));
    }

    #[test]
    fn hid_help_explains_its_narrow_hardware_boundary() {
        let help = help_for(&["hid"]);
        assert!(help.contains("read-only"));
        assert!(help.contains("ordinary keyboard"));
        assert!(help.contains("control Herdr agents"));
    }

    #[derive(Default)]
    struct RecordingDirectory {
        writes: RefCell<Vec<(String, String)>>,
        pane_gone: bool,
    }

    impl PaneDirectory for RecordingDirectory {
        fn panes(&self) -> Result<Vec<beckon::core::Pane>> {
            Ok(Vec::new())
        }

        fn write_fkey(&self, _pane_id: &str, _key: Option<&str>) -> Result<()> {
            Ok(())
        }

        fn write_presentation_tokens(
            &self,
            pane_id: &str,
            binding: &str,
        ) -> Result<PresentationTokenWrite> {
            self.writes
                .borrow_mut()
                .push((pane_id.into(), binding.into()));
            Ok(if self.pane_gone {
                PresentationTokenWrite::PaneGone
            } else {
                PresentationTokenWrite::Written
            })
        }

        fn focus_pane(&self, _pane_id: &str) -> Result<()> {
            Ok(())
        }

        fn send_keys(&self, _pane_id: &str, _keys: &[&str]) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn presentation_tokens_publish_once_and_on_binding_change() {
        let directory = RecordingDirectory::default();
        let mut publisher = PresentationPublisher::default();
        let pane = |binding: &str| PanePresentation {
            pane_id: "w:p1".into(),
            title: "task".into(),
            binding: binding.into(),
            agent_status: "idle".into(),
            focused: false,
        };

        assert!(
            publisher
                .publish(&directory, vec![pane("unbound")])
                .unwrap()
        );
        assert!(
            !publisher
                .publish(&directory, vec![pane("unbound")])
                .unwrap()
        );
        assert!(publisher.publish(&directory, vec![pane("F4")]).unwrap());
        assert_eq!(
            *directory.writes.borrow(),
            vec![
                ("w:p1".into(), "unbound".into()),
                ("w:p1".into(), "F4".into()),
            ]
        );
    }

    #[test]
    fn presentation_token_pane_gone_is_an_expected_close_race() {
        let directory = RecordingDirectory {
            pane_gone: true,
            ..Default::default()
        };
        let mut publisher = PresentationPublisher::default();
        let pane = PanePresentation {
            pane_id: "w:p1".into(),
            title: "closing task".into(),
            binding: "F2".into(),
            agent_status: "working".into(),
            focused: false,
        };

        assert!(!publisher.publish(&directory, vec![pane.clone()]).unwrap());
        assert!(!publisher.publish(&directory, vec![pane]).unwrap());
        assert_eq!(
            *directory.writes.borrow(),
            vec![("w:p1".into(), "F2".into())]
        );
    }

    #[test]
    fn parses_comma_separated_hid_states() {
        let cli = Cli::try_parse_from([
            "beckon",
            "hid",
            "send",
            "--sequence",
            "2",
            "--states",
            "working,unknown,unbound,unbound,unbound,unbound,unbound,unbound,unbound,unbound",
        ])
        .unwrap();
        let CommandLine::Hid {
            command: HidCommand::Send(args),
        } = cli.command
        else {
            panic!("expected HID send command");
        };
        assert_eq!(args.sequence, 2);
        assert_eq!(args.states.len(), hid::SLOT_COUNT);
    }

    #[test]
    fn parses_release_by_key_without_a_pane() {
        let cli = Cli::try_parse_from(["beckon", "release", "--key", "f2"]).unwrap();
        let CommandLine::Release(args) = cli.command else {
            panic!("expected release command");
        };
        assert_eq!(args.key.as_deref(), Some("f2"));
        assert!(args.pane.pane.is_none());
    }

    #[test]
    fn describes_enabled_input_profiles_at_startup() {
        assert_eq!(
            input_diagnostic(&[InputProfile::Glove80, InputProfile::MacbookFunctionKeys]),
            "glove80, macbook-function-keys"
        );
    }
}
