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
        AgentPresentation, AgentRef, AgentSource, BindingService, BindingState, BindingStore,
        PresentationTokenWrite, STATE_VERSION,
    },
    display::DisplaySet,
    focus::{CommandFocus, FocusAdapter, FocusContext},
    herdr::{HerdrCli, LivePaneDirectory, discover_sessions},
    hid::{self, Status, StatusSnapshot},
    input::{
        Glove80HotkeyInput, InputAdapter, MacbookFunctionKeyInput, RegisteredInput,
        register_adapters,
    },
    session::SessionRouter,
    state::JsonBindingStore,
    terminal::{self, SurfaceHandle, SurfaceRecord, TerminalLink},
};
use clap::{Args, Parser, Subcommand};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder};
use tracing::{debug, info, info_span, warn};
use tracing_subscriber::EnvFilter;

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
key. Beckon never auto-registers panes or agents.\n\
\n\
Beckon manages every live Herdr session it discovers. A pane that exists in\n\
more than one session requires `--session <name>`; a unique pane ID resolves\n\
on its own."
    )]
    Bind(BindArgs),
    /// Clear a pane's Beckon binding. Defaults to $HERDR_PANE_ID; --key works from any pane.
    #[command(
        long_about = "Remove a Beckon registration. Run this in the bound pane, provide\n\
`--pane <pane-id>`, or use `--key f1` through `--key f10` from anywhere.\n\
For a pane ID that exists in more than one Herdr session, add `--session`.\n\
This changes only the local Beckon binding and the pane's visible fkey token; it\n\
does not close a pane or control an agent."
    )]
    Release(ReleaseArgs),
    /// Print every live Herdr pane with its resolved title and Beckon binding.
    Status,
    /// List the terminal surfaces the configured backend can see.
    #[command(
        long_about = "List the terminal surfaces the configured [terminal] backend can see, marking
\
adopted ones. This is read-only: Beckon never infers which surface displays
\
which session."
    )]
    Terminals(TerminalsArgs),
    /// Record which terminal surface displays a Herdr session.
    #[command(
        long_about = "Explicitly record that one terminal surface (from `beckon terminals`)\n\
displays one Herdr session. Beckon raises that surface before focusing a bound\n\
pane in the session. The record lives in the state directory, not in\n\
configuration, and is only ever changed by this command or `beckon forget`."
    )]
    Adopt(AdoptArgs),
    /// Remove an adopted terminal surface for a session.
    #[command(
        long_about = "Remove the adopted surface record for one session. This works without\n\
a configured backend so it can clean up stale records even after the terminal\n\
setup changed."
    )]
    Forget(ForgetArgs),
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
    /// Focus the agent bound to a logical key, as a pad press would.
    Focus(FocusArgs),
    /// Drive the Creator Micro 2 indicator keys and ring.
    CreatorMicro {
        #[command(subcommand)]
        command: CreatorMicroCommand,
    },
}

#[derive(Args)]
struct FocusArgs {
    /// Logical Beckon slot, for example f4.
    #[arg(long)]
    key: String,
}

#[derive(Subcommand)]
enum CreatorMicroCommand {
    /// Paint every agent state, then highlight a key when it is pressed.
    Demo,
    /// Turn every indicator and the ring off.
    Off,
    /// Log every HID key notification, including non-AG macro keys.
    Listen,
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
    /// Herdr session owning the pane. Required only when the pane ID exists in
    /// more than one session.
    #[arg(long)]
    session: Option<String>,
}

#[derive(Args)]
struct ReleaseArgs {
    /// Clear every Beckon binding. This does not close panes or control agents.
    #[arg(long, conflicts_with_all = ["key", "pane"])]
    all: bool,
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

#[derive(Args)]
struct TerminalsArgs {
    /// Emit surfaces as JSON for scripts.
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct AdoptArgs {
    /// Herdr session whose surface this is.
    #[arg(long)]
    session: String,
    /// Surface handle exactly as printed by `beckon terminals`.
    #[arg(long)]
    terminal: String,
}

#[derive(Args)]
struct ForgetArgs {
    /// Herdr session whose surface record should be removed.
    #[arg(long)]
    session: String,
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
        #[serde(default)]
        session: Option<String>,
        key: Option<String>,
        /// Integration that owns the agent. Omitted by older CLI clients.
        #[serde(default)]
        source: Option<String>,
    },
    ReleasePane {
        pane_id: String,
        #[serde(default)]
        session: Option<String>,
        #[serde(default)]
        source: Option<String>,
    },
    ReleaseKey {
        key: String,
    },
    ReleaseAll,
    Status,
    FocusKey {
        key: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Response {
    Ok { data: Value },
    Error { message: String },
}

fn main() -> Result<()> {
    initialize_tracing();
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
        CommandLine::Bind(args) => {
            let config = config::load()?;
            let (source, session, pane_id) = match bind_identity(&config) {
                Some(identity) => (
                    Some(identity.source),
                    Some(identity.session),
                    identity.pane_id,
                ),
                None => (None, args.pane.session, current_pane(args.pane.pane)?),
            };
            client(Request::Bind {
                pane_id,
                session,
                key: args.key,
                source,
            })
        }
        CommandLine::Release(args) if args.all => client(Request::ReleaseAll),
        CommandLine::Release(args) => match args.key {
            Some(key) => {
                if args.pane.pane.is_some() {
                    bail!("--key and --pane cannot be used together");
                }
                client(Request::ReleaseKey { key })
            }
            None => {
                let config = config::load()?;
                let (source, session, pane_id) = match bind_identity(&config) {
                    Some(identity) => (
                        Some(identity.source),
                        Some(identity.session),
                        identity.pane_id,
                    ),
                    None => (None, args.pane.session, current_pane(args.pane.pane)?),
                };
                client(Request::ReleasePane {
                    pane_id,
                    session,
                    source,
                })
            }
        },
        CommandLine::Status => client(Request::Status),
        CommandLine::Terminals(args) => terminals_command(args),
        CommandLine::Adopt(args) => adopt_command(args),
        CommandLine::Forget(args) => forget_command(args),
        CommandLine::Preview(args) => preview(args),
        CommandLine::Hid { command } => hid_command(command),
        CommandLine::ListenKeys => listen_keys(),
        CommandLine::CreatorMicro { command } => creator_micro_command(command),
        CommandLine::Focus(args) => client(Request::FocusKey { key: args.key }),
    }
}

/// Beckon needs Tao's main-thread event loop for global hotkeys, but it has no
/// windows or user-facing app surface. Accessory policy keeps a launchd-managed
/// daemon out of the Dock and application switcher.
fn background_event_loop() -> EventLoop<()> {
    let mut event_loop = EventLoopBuilder::new().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};

        event_loop.set_activation_policy(ActivationPolicy::Accessory);
        event_loop.set_activate_ignoring_other_apps(false);
    }
    event_loop
}

/// Beckond writes structured diagnostics to stderr, which Home Manager's
/// launchd service persists in its configured `beckond.error.log`. `BECKON_LOG`
/// accepts normal EnvFilter values, e.g. `BECKON_LOG=debug` while diagnosing an
/// input issue. Keeping diagnostics opt-in avoids noisy per-key logs normally.
fn initialize_tracing() {
    let filter =
        EnvFilter::try_from_env("BECKON_LOG").unwrap_or_else(|_| EnvFilter::new("beckon=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_target(false)
        .compact()
        .init();
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

/// Interactive Creator Micro check. Beckon stays the only writer: this paints
/// a deterministic render plan and then highlights whichever AG key was
/// pressed, so the transport, slot mapping and HID notifications are all
/// exercised without needing a live Herdr or Rex source.
fn creator_micro_command(command: CreatorMicroCommand) -> Result<()> {
    use beckon::creator_micro::{self, Pad};
    use beckon::render::{AgentState, KeyRender, Motion};

    let config = config::load()?;
    let pad_config = config.outputs.creator_micro.clone();
    let mut pad = Pad::open()?;

    match command {
        CreatorMicroCommand::Off => {
            creator_micro::clear(&mut pad, &pad_config)?;
            println!("creator-micro: all indicators and ring off");
            return Ok(());
        }
        CreatorMicroCommand::Listen => {
            println!("creator-micro listen: press any pad key, including the mic key.");
            println!("Ctrl-C to stop. No lighting is written.");
            loop {
                if let Some(event) = pad.poll_event()? {
                    let action = if event.pressed { "PRESS" } else { "RELEASE" };
                    let agent = event
                        .agent
                        .map(|value| format!("ag={value}"))
                        .unwrap_or_else(|| "ag=-".into());
                    println!("  {action:<7} {:<16} {agent}", event.key);
                }
            }
        }
        CreatorMicroCommand::Demo => {}
    }

    let plan = beckon::render::demo_plan(&config.display)?;
    creator_micro::write_plan(&mut pad, &plan, &pad_config)?;

    println!("creator-micro demo: six slots painted");
    for (index, slot) in pad_config.slots.iter().enumerate() {
        let key = plan
            .keys
            .get(index)
            .map_or("?", |render| render.key.as_str());
        let state = plan
            .keys
            .get(index)
            .and_then(|render| render.state)
            .map_or("unbound".to_string(), |state| {
                format!("{state:?}").to_lowercase()
            });
        println!("  {key} <- AG{slot:02}  {state}");
    }
    println!("Press and hold any of the six top keys. Leave this running.");
    println!("Ctrl-C stops the writer; the pad then goes dark by design.");

    // AIDEV-NOTE: the firmware drops AG slot overrides when the writing HID
    // host closes its handle, and ChatGPT rewrites the global keys zone. Keep
    // re-asserting the frame so the lights survive while this command runs.
    let mut current_plan = plan.clone();
    let mut last_write = Instant::now();
    let refresh_every = Duration::from_secs(2);
    loop {
        if let Some(event) = pad.poll_event()? {
            let Some(slot) = event.slot() else {
                continue;
            };
            let Some(index) = pad_config
                .slots
                .iter()
                .position(|candidate| *candidate == slot)
            else {
                continue;
            };
            let logical = plan.keys.get(index).map_or("?", |key| key.key.as_str());
            current_plan = plan.clone();
            if event.pressed {
                println!("  HOLD    {logical} ({})", event.key);
                if let Some(render) = current_plan.keys.get_mut(index) {
                    let done = plan.treatments.for_state(AgentState::Done);
                    *render = KeyRender {
                        key: render.key.clone(),
                        state: Some(AgentState::Done),
                        color: done.color,
                        brightness: 1.0,
                        motion: Motion::Pulse,
                    };
                }
            } else {
                println!("  RELEASE {logical} ({})", event.key);
            }
            creator_micro::write_plan(&mut pad, &current_plan, &pad_config)?;
            last_write = Instant::now();
        }
        if last_write.elapsed() >= refresh_every {
            creator_micro::write_plan(&mut pad, &current_plan, &pad_config)?;
            last_write = Instant::now();
        }
    }
}

fn preview(args: PreviewArgs) -> Result<()> {
    let config = config::load()?;
    let plan = if args.all_states {
        beckon::render::all_state_examples(&config.display)?
    } else {
        let store = JsonBindingStore::from_environment();
        let sources = cli_sources(&config)?;
        let bindings = BindingService::new(&store, &sources);
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

/// List the terminal surfaces the configured backend can see, marking adopted
/// ones. Read-only: adoption is always an explicit `beckon adopt`.
fn terminals_command(args: TerminalsArgs) -> Result<()> {
    let config = config::load()?;
    let link = terminal_link(&config)?;
    let surfaces = link.backend().list_surfaces()?;
    let adopted = link
        .store()
        .load()?
        .surfaces
        .into_iter()
        .map(|record| (record.handle, record.session))
        .collect::<BTreeMap<_, _>>();
    if args.json {
        let entries = surfaces
            .iter()
            .map(|surface| {
                json!({
                    "handle": surface.handle.as_str(),
                    "title": surface.title,
                    "window": surface.window,
                    "tab": surface.tab,
                    "adopted_session": adopted.get(&surface.handle),
                })
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "backend": link.backend().id(),
                "surfaces": entries,
            }))?
        );
        return Ok(());
    }
    if surfaces.is_empty() {
        println!("backend {} lists no terminal surfaces", link.backend().id());
        return Ok(());
    }
    for surface in surfaces {
        let adopted = adopted
            .get(&surface.handle)
            .map(|session| format!(" (session {session})"))
            .unwrap_or_default();
        println!(
            "{}\t{}{}\t{}/{}",
            surface.handle, surface.title, adopted, surface.window, surface.tab
        );
    }
    Ok(())
}

/// Record an explicit session-to-surface association. The handle must be
/// currently listed, so typos and stale handles cannot be adopted silently.
fn adopt_command(args: AdoptArgs) -> Result<()> {
    let config = config::load()?;
    let link = terminal_link(&config)?;
    let handle = SurfaceHandle::new(&args.terminal);
    let surfaces = link.backend().list_surfaces()?;
    if !surfaces.iter().any(|surface| surface.handle == handle) {
        let known = surfaces
            .iter()
            .map(|surface| surface.handle.as_str())
            .collect::<Vec<_>>();
        let known = if known.is_empty() {
            "none currently listed".to_string()
        } else {
            known.join(", ")
        };
        bail!(
            "backend {} does not list surface {}; `beckon terminals` shows: {known}",
            link.backend().id(),
            handle
        );
    }
    link.store().record(SurfaceRecord {
        backend: link.backend().id().to_string(),
        session: args.session.clone(),
        handle: handle.clone(),
    })?;
    println!("adopted {handle} for session {}", args.session);
    Ok(())
}

/// Remove an adopted record. Deliberately independent of the configured
/// backend: cleanup must work even after the terminal setup changed.
fn forget_command(args: ForgetArgs) -> Result<()> {
    let store = terminal::SurfaceStore::from_environment();
    if store.forget(&args.session)? {
        println!("forgot the adopted surface for session {}", args.session);
    } else {
        println!("no adopted surface for session {}", args.session);
    }
    Ok(())
}

fn terminal_link(config: &config::Config) -> Result<TerminalLink> {
    match terminal::from_config(&config.terminal)? {
        Some(link) => Ok(link),
        None => bail!(
            "no terminal backend is configured; set [terminal] backend in {}",
            config::path().display()
        ),
    }
}

fn listen_keys() -> Result<()> {
    // Keep this diagnostic usable before `beckon init`: if configuration is
    // absent it tests the default Glove80 profile. An existing config is still
    // fully validated before its input profiles are used.
    let input_profiles = if config::path().exists() {
        config::load()?.input.enabled_profiles()?
    } else {
        vec![InputProfile::default()]
    };
    let event_loop = background_event_loop();
    let manager = GlobalHotKeyManager::new().context("initialize macOS global hotkeys")?;
    let input = register_input(&input_profiles, &manager)?;
    info!(profiles = %input_diagnostic(&input_profiles), "input listener registered shortcuts");
    eprintln!("beckon inputs: {}", input_diagnostic(&input_profiles));
    eprintln!("Listening for Beckon F keys. Press Control-C to stop.");
    eprintln!("Logging presses to {}", key_event_log_path().display());
    let receiver = GlobalHotKeyEvent::receiver();
    event_loop.run(move |_event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        while let Ok(event) = receiver.try_recv() {
            debug!(hotkey_id = event.id, state = ?event.state, "received global hotkey event");
            let Some(binding) = input.pressed(&event) else {
                debug!(hotkey_id = event.id, "ignored unowned global hotkey event");
                continue;
            };
            info!(
                key = binding.key,
                physical = binding.description,
                "routed input to logical key"
            );
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

/// Resolve `beckon bind` from a configured source's identity environment
/// variables before falling back to Herdr's `HERDR_PANE_ID`. Rex terminals
/// export `REX_SESSION` / `REX_BLOCK`, which are declared in `[sources]`.
fn bind_identity(config: &config::Config) -> Option<AgentRef> {
    config
        .sources
        .iter()
        .map(|source| beckon::command_source::CommandSource::new(source.clone()))
        .find_map(|source| AgentSource::current_identity(&source))
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
    let sources = build_sources(&config)?;
    info!(
        sources = %config.sources.iter().map(|s| s.id.as_str()).collect::<Vec<_>>().join(", "),
        "daemon configured agent sources"
    );
    let terminal = terminal::from_config(&config.terminal)?;
    if let Some(link) = &terminal {
        info!(
            backend = link.backend().id(),
            "terminal surface backend configured"
        );
    }
    let output_ids = config.outputs.ids();
    let mut displays = DisplaySet::from_config(&config.outputs)?;
    let mut last_render_error = None;
    let mut presentation = PresentationPublisher::default();
    let mut last_presentation_error = None;
    let event_loop = background_event_loop();
    let manager = GlobalHotKeyManager::new().context("initialize macOS global hotkeys")?;
    let input_profiles = config.input.enabled_profiles()?;
    let input = register_input(&input_profiles, &manager)?;
    info!(profiles = %input_diagnostic(&input_profiles), socket = %path.display(), "daemon registered input shortcuts");
    eprintln!("beckond inputs: {}", input_diagnostic(&input_profiles));
    info!(
        outputs = %output_ids.join(", "),
        "daemon configured display outputs"
    );
    let receiver = GlobalHotKeyEvent::receiver();
    let mut confirm = RepeatPressConfirm::default();
    event_loop.run(move |_event, _, control_flow| {
        *control_flow =
            ControlFlow::WaitUntil(std::time::Instant::now() + Duration::from_millis(50));
        while let Ok((stream, _)) = listener.accept() {
            if let Err(error) = handle_connection(stream, &sources) {
                eprintln!("request failed: {error:#}");
            }
        }
        match publish_display(&config, &sources, &mut displays) {
            Ok(failures) => {
                last_render_error = None;
                for failure in failures {
                    eprintln!(
                        "update {} status display: {}",
                        failure.adapter, failure.message
                    );
                }
            }
            Err(error) => {
                let error = format!("{error:#}");
                if last_render_error.as_deref() != Some(error.as_str()) {
                    eprintln!("render status display: {error}");
                    last_render_error = Some(error);
                }
            }
        }
        match presentation.sync(&sources) {
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
            debug!(hotkey_id = event.id, state = ?event.state, "received global hotkey event");
            if let Some(binding) = input.pressed(&event) {
                let key = binding.key;
                info!(
                    key,
                    physical = binding.description,
                    "routed input to logical key"
                );
                if let Err(error) =
                    append_key_event(&key_event_line(key, binding.description, "daemon"))
                {
                    eprintln!("record {key}: {error:#}");
                }
                let now = Instant::now();
                let target = pane_for_key(key, &sources);
                let confirmed = config.actions.confirm.enabled
                    && target.as_ref().is_ok_and(|pane| {
                        confirm.take_if_ready(key, pane, pane_is_focused(&sources, pane), now)
                    });
                if confirmed {
                    info!(key, "repeat press confirmed configured action");
                    let keys = config
                        .actions
                        .confirm
                        .keys
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>();
                    let result = target.and_then(|pane| sources.send_keys(&pane, &keys));
                    match result {
                        Ok(()) => {
                            info!(key, "configured action completed");
                            if let Err(error) =
                                append_key_event(&focus_result_line(key, "confirm-ok"))
                            {
                                eprintln!("record confirmation {key}: {error:#}");
                            }
                        }
                        Err(error) => {
                            warn!(key, error = %format!("{error:#}"), "configured action failed");
                            if let Err(record_error) =
                                append_key_event(&focus_result_line(key, "confirm-error"))
                            {
                                eprintln!("record confirmation {key}: {record_error:#}");
                            }
                            eprintln!("confirm {key}: {error:#}");
                        }
                    }
                } else {
                    match focus_key(key, &config, &sources, terminal.as_ref()) {
                        Ok(()) => {
                            info!(key, "navigation completed");
                            if config.actions.confirm.enabled
                                && let Ok(pane) = target
                            {
                                confirm.arm(
                                    key,
                                    &pane,
                                    Duration::from_millis(config.actions.confirm.repeat_press_ms),
                                    now,
                                );
                            }
                            if let Err(error) = append_key_event(&focus_result_line(key, "ok")) {
                                eprintln!("record focus {key}: {error:#}");
                            }
                        }
                        Err(error) => {
                            warn!(key, error = %format!("{error:#}"), "navigation failed");
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
/// binding changes. Agent references are immutable, so including them in the
/// one update keeps the sidebar independent of missing-token fallbacks.
#[derive(Default)]
struct PresentationPublisher {
    published_bindings: BTreeMap<String, String>,
}

impl PresentationPublisher {
    fn sync<D: AgentSource>(&mut self, panes: &D) -> Result<bool> {
        let store = JsonBindingStore::from_environment();
        let presentation = BindingService::new(&store, panes).panes()?;
        self.publish(panes, presentation)
    }

    fn publish<D: AgentSource>(
        &mut self,
        panes: &D,
        presentation: Vec<AgentPresentation>,
    ) -> Result<bool> {
        let mut changed = false;
        let live = presentation
            .iter()
            .map(|pane| pane_reference(pane).to_string())
            .collect::<std::collections::BTreeSet<_>>();
        self.published_bindings
            .retain(|reference, _| live.contains(reference.as_str()));
        for pane in presentation {
            let reference = pane_reference(&pane);
            let key = reference.to_string();
            if self.published_bindings.get(&key) == Some(&pane.binding) {
                continue;
            }
            match panes.write_presentation_tokens(&reference, &pane.binding)? {
                PresentationTokenWrite::Written => {
                    self.published_bindings.insert(key, pane.binding);
                    changed = true;
                }
                PresentationTokenWrite::PaneGone => {
                    // A cache snapshot may briefly outlive a pane. Remember the
                    // definitive server response to avoid retrying on every
                    // event-loop tick; `retain` removes it after reconciliation.
                    self.published_bindings.insert(key, pane.binding);
                }
            }
        }
        Ok(changed)
    }
}

fn pane_reference(pane: &AgentPresentation) -> AgentRef {
    AgentRef::for_source(
        pane.source.clone(),
        pane.session.clone(),
        pane.pane_id.clone(),
    )
}

/// Read the durable binding ledger without mutating it, combine it with the
/// live pane cache, and send a new transport snapshot only when it matters.
/// Binding reconciliation remains a CLI/request operation; polling it here
/// would rewrite the state file and Herdr metadata on every event-loop tick.
fn publish_display<D>(
    config: &config::Config,
    panes: &D,
    displays: &mut DisplaySet,
) -> Result<Vec<beckon::display::DisplayFailure>>
where
    D: AgentSource,
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
                .find(|pane| binding.matches_agent(pane))
                .cloned()
                .map(|pane| (binding, pane))
        })
        .collect::<Vec<_>>();
    let plan = beckon::render::render(&config.display, &bindings)?;
    Ok(displays.publish(&plan))
}

fn handle_connection<D: AgentSource>(mut stream: UnixStream, panes: &D) -> Result<()> {
    configure_client_stream(&stream)?;
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

fn configure_client_stream(stream: &UnixStream) -> Result<()> {
    // AIDEV-NOTE: The nonblocking listener yields nonblocking accepted streams
    // on macOS. Large status responses otherwise stop at the socket buffer
    // boundary with EAGAIN. Bound blocking I/O keeps responses atomic without
    // allowing a stalled local client to freeze the main-thread event loop.
    stream
        .set_nonblocking(false)
        .context("make accepted Beckon client socket blocking")?;
    let timeout = Some(Duration::from_secs(1));
    stream
        .set_read_timeout(timeout)
        .context("set Beckon client socket read timeout")?;
    stream
        .set_write_timeout(timeout)
        .context("set Beckon client socket write timeout")?;
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

fn dispatch<D: AgentSource>(request: Request, panes: &D) -> Result<Value> {
    let store = JsonBindingStore::from_environment();
    let bindings = BindingService::new(&store, panes);
    match request {
        Request::Bind {
            pane_id,
            session,
            key,
            source,
        } => {
            let pane = resolve_pane(panes, session, &pane_id, source.as_deref())?;
            Ok(serde_json::to_value(bindings.bind(&pane, key.as_deref())?)?)
        }
        Request::ReleasePane {
            pane_id,
            session,
            source,
        } => {
            let pane = resolve_pane(panes, session, &pane_id, source.as_deref())?;
            Ok(json!({
                "session": pane.session,
                "pane_id": pane.pane_id,
                "changed": bindings.release(&pane)?,
            }))
        }
        Request::ReleaseKey { key } => {
            let released = bindings.release_key(&key)?;
            Ok(json!({
                "key": key,
                "session": released.as_ref().map(|binding| binding.session.clone()),
                "pane_id": released.as_ref().map(|binding| binding.pane_id.clone()),
                "changed": released.is_some(),
            }))
        }
        Request::ReleaseAll => {
            let released = bindings.release_all()?;
            Ok(json!({"released": released.len(), "bindings": released}))
        }
        Request::FocusKey { key } => {
            let pane = bindings.pane_for_key(&key)?;
            panes.focus_pane(&pane)?;
            Ok(json!({
                "key": key,
                "source": pane.source,
                "scope": pane.session,
                "id": pane.pane_id,
            }))
        }
        Request::Status => Ok(json!({
            "bindings": bindings.status()?.into_iter().map(|(binding, pane)| json!({
                "key": binding.key,
                "source": binding.source,
                "session": binding.session,
                "pane": pane,
            })).collect::<Vec<_>>(),
            "panes": bindings.panes()?,
        })),
    }
}

/// Resolve a client-supplied pane reference. Without an explicit `--session`,
/// the pane ID must exist in exactly one managed session; ambiguity asks for
/// `--session` instead of guessing.
fn resolve_pane<D: AgentSource>(
    panes: &D,
    session: Option<String>,
    pane_id: &str,
    source: Option<&str>,
) -> Result<AgentRef> {
    let source = source.unwrap_or(beckon::core::DEFAULT_SOURCE);
    if let Some(session) = session {
        return Ok(AgentRef::for_source(source, session, pane_id));
    }
    let matches = panes
        .panes()?
        .into_iter()
        .filter(|pane| pane.source == source && pane.pane_id == pane_id)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => bail!("pane {pane_id} no longer exists in any managed session"),
        [pane] => Ok(pane.reference()),
        many => {
            let sessions = many
                .iter()
                .map(|pane| pane.session.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            bail!("pane {pane_id} exists in multiple sessions ({sessions}); retry with --session")
        }
    }
}

/// Build every configured agent integration as one routing source set.
/// Herdr is optional once at least one `[sources]` entry exists, so a Rex-only
/// setup can still drive the pad when Herdr is not running.
fn build_sources(config: &config::Config) -> Result<beckon::source_set::SourceSet> {
    let mut sources: Vec<Box<dyn AgentSource>> = Vec::new();
    match live_directories(config) {
        Ok(herdr) => sources.push(Box::new(herdr)),
        Err(error) if config.sources.is_empty() => return Err(error),
        Err(error) => {
            eprintln!("beckond: Herdr unavailable, continuing with configured sources: {error:#}")
        }
    }
    for source in &config.sources {
        sources.push(Box::new(beckon::command_source::CommandSource::new(
            source.clone(),
        )));
    }
    Ok(beckon::source_set::SourceSet::new(sources))
}

/// One-shot equivalent for read-only CLI commands such as `beckon preview`.
fn cli_sources(config: &config::Config) -> Result<beckon::source_set::SourceSet> {
    let mut sources: Vec<Box<dyn AgentSource>> = Vec::new();
    match cli_directories(config) {
        Ok(herdr) => sources.push(Box::new(herdr)),
        Err(error) if config.sources.is_empty() => return Err(error),
        Err(error) => eprintln!("beckon: Herdr unavailable: {error:#}"),
    }
    for source in &config.sources {
        sources.push(Box::new(beckon::command_source::CommandSource::new(
            source.clone(),
        )));
    }
    Ok(beckon::source_set::SourceSet::new(sources))
}

/// Build the live session router used by the daemon.
fn live_directories(config: &config::Config) -> Result<SessionRouter<LivePaneDirectory>> {
    let sessions = discover_sessions(
        config.herdr.socket.as_deref(),
        config.herdr.allowed_sessions(),
    );
    if sessions.is_empty() {
        bail!("no reachable Herdr sessions; start Herdr or check the [herdr] configuration");
    }
    let mut directories = std::collections::BTreeMap::new();
    for session in sessions {
        match LivePaneDirectory::start(session.clone(), &config.herdr.cli) {
            Ok(directory) => {
                directories.insert(session.name.clone(), directory);
            }
            Err(error) => {
                eprintln!("beckond: skipping session {}: {error:#}", session.name);
            }
        }
    }
    if directories.is_empty() {
        bail!("no Herdr session accepted a connection; check the [herdr] configuration");
    }
    Ok(SessionRouter::new(directories))
}

/// Build the one-shot CLI session router used by read-only commands.
fn cli_directories(config: &config::Config) -> Result<SessionRouter<HerdrCli>> {
    let sessions = discover_sessions(
        config.herdr.socket.as_deref(),
        config.herdr.allowed_sessions(),
    );
    if sessions.is_empty() {
        bail!("no reachable Herdr sessions; start Herdr or check the [herdr] configuration");
    }
    let directories = sessions
        .into_iter()
        .map(|session| {
            let name = session.name.clone();
            (name, HerdrCli::for_session(&config.herdr.cli, &session))
        })
        .collect();
    Ok(SessionRouter::new(directories))
}

fn focus_key<D: AgentSource>(
    key: &str,
    config: &config::Config,
    panes: &D,
    terminal: Option<&TerminalLink>,
) -> Result<()> {
    let span = info_span!("focus_bound_pane", key);
    let _entered = span.enter();
    let pane = pane_for_key(key, panes)?;
    info!(session = %pane.session, pane_id = %pane.pane_id, "resolved bound pane");
    // Raising the adopted surface is best effort: a missing record, a changed
    // backend, or a stale handle warns and degrades to command + pane focus.
    // Beckon never guesses a replacement surface.
    // AIDEV-NOTE: the legacy [focus] command and adopted terminal surface are
    // Herdr's window-manager integration. Other sources bring their own host
    // forward through their configured `activate` command inside focus_pane,
    // so running the Herdr path here would raise the wrong application.
    let herdr = pane.source == beckon::core::DEFAULT_SOURCE;
    let mut raised = None;
    if herdr && let Some(link) = terminal {
        match link.raise_for_session(&pane.session) {
            Ok(outcome) => raised = outcome,
            Err(error) => {
                let error = format!("{error:#}");
                warn!(session = %pane.session, error = %error, "adopted terminal surface not raised");
                eprintln!("focus {key}: {error}");
            }
        }
    }
    if herdr {
        let context = FocusContext {
            key,
            pane: &pane,
            terminal_handle: raised.as_ref(),
        };
        CommandFocus::new(&config.focus).focus_terminal(&context)?;
        info!("terminal focus integration completed");
    }
    debug!(pane = %pane, source = %pane.source, "requesting agent focus");
    panes.focus_pane(&pane)?;
    info!(pane = %pane, "agent focus completed");
    Ok(())
}

fn pane_for_key<D: AgentSource>(key: &str, panes: &D) -> Result<AgentRef> {
    let store = JsonBindingStore::from_environment();
    BindingService::new(&store, panes).pane_for_key(key)
}

fn pane_is_focused<D: AgentSource>(panes: &D, pane: &AgentRef) -> bool {
    panes
        .panes()
        .map(|panes| {
            panes
                .into_iter()
                .any(|candidate| candidate.matches(pane) && candidate.focused)
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, thread};

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
        assert!(bind_help.contains("--session <name>"));

        let release_help = help_for(&["release"]);
        assert!(release_help.contains("does not close a pane or control an agent"));
        assert!(release_help.contains("--session"));
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
        panes: Vec<beckon::core::Agent>,
    }

    impl AgentSource for RecordingDirectory {
        fn id(&self) -> &str {
            beckon::core::DEFAULT_SOURCE
        }

        fn panes(&self) -> Result<Vec<beckon::core::Agent>> {
            Ok(self.panes.clone())
        }

        fn observed_sessions(&self) -> Result<std::collections::BTreeSet<String>> {
            Ok(self.panes.iter().map(|pane| pane.session.clone()).collect())
        }

        fn write_fkey(&self, _pane: &AgentRef, _key: Option<&str>) -> Result<()> {
            Ok(())
        }

        fn write_presentation_tokens(
            &self,
            pane: &AgentRef,
            binding: &str,
        ) -> Result<PresentationTokenWrite> {
            self.writes
                .borrow_mut()
                .push((pane.to_string(), binding.into()));
            Ok(if self.pane_gone {
                PresentationTokenWrite::PaneGone
            } else {
                PresentationTokenWrite::Written
            })
        }

        fn focus_pane(&self, _pane: &AgentRef) -> Result<()> {
            Ok(())
        }

        fn send_keys(&self, _pane: &AgentRef, _keys: &[&str]) -> Result<()> {
            Ok(())
        }
    }

    fn pane(session: &str, id: &str, focused: bool) -> beckon::core::Agent {
        beckon::core::Agent {
            source: beckon::core::DEFAULT_SOURCE.into(),
            pane_id: id.into(),
            session: session.into(),
            revision: 0,
            agent_status: "idle".into(),
            agent: None,
            label: None,
            cwd: None,
            terminal_title: None,
            terminal_title_stripped: None,
            focused,
            tokens: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn resolves_an_unambiguous_pane_across_sessions() {
        let directory = RecordingDirectory {
            panes: vec![pane("agent-workspace", "w6:p1", false)],
            ..Default::default()
        };
        assert_eq!(
            resolve_pane(&directory, None, "w6:p1", None).unwrap(),
            AgentRef::new("agent-workspace", "w6:p1")
        );
    }

    #[test]
    fn rejects_missing_or_ambiguous_panes_with_actionable_messages() {
        let directory = RecordingDirectory {
            panes: vec![
                pane("default", "w1:p1", false),
                pane("agent-workspace", "w1:p1", false),
            ],
            ..Default::default()
        };

        let ambiguous = resolve_pane(&directory, None, "w1:p1", None)
            .unwrap_err()
            .to_string();
        assert!(ambiguous.contains("multiple sessions"), "{ambiguous}");
        assert!(ambiguous.contains("--session"), "{ambiguous}");

        let missing = resolve_pane(&directory, None, "gone", None)
            .unwrap_err()
            .to_string();
        assert!(
            missing.contains("no longer exists in any managed session"),
            "{missing}"
        );

        // An explicit session bypasses the search entirely.
        assert_eq!(
            resolve_pane(&directory, Some("agent-workspace".into()), "w1:p1", None).unwrap(),
            AgentRef::new("agent-workspace", "w1:p1")
        );
    }

    #[test]
    fn presentation_tokens_publish_once_and_on_binding_change() {
        let directory = RecordingDirectory::default();
        let mut publisher = PresentationPublisher::default();
        let pane = |binding: &str| AgentPresentation {
            source: beckon::core::DEFAULT_SOURCE.into(),
            session: "default".into(),
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
                ("herdr:default:w:p1".into(), "unbound".into()),
                ("herdr:default:w:p1".into(), "F4".into()),
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
        let pane = AgentPresentation {
            source: beckon::core::DEFAULT_SOURCE.into(),
            session: "default".into(),
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
            vec![("herdr:default:w:p1".into(), "F2".into())]
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
    fn parses_explicit_release_all_without_a_pane() {
        let cli = Cli::try_parse_from(["beckon", "release", "--all"]).unwrap();
        let CommandLine::Release(args) = cli.command else {
            panic!("expected release command");
        };
        assert!(args.all);
        assert!(args.key.is_none());
        assert!(args.pane.pane.is_none());
    }

    #[test]
    fn rejects_release_all_with_a_target() {
        assert!(Cli::try_parse_from(["beckon", "release", "--all", "--key", "f2"]).is_err());
        assert!(Cli::try_parse_from(["beckon", "release", "--all", "--pane", "w8:p1"]).is_err());
    }

    #[test]
    fn describes_enabled_input_profiles_at_startup() {
        assert_eq!(
            input_diagnostic(&[InputProfile::Glove80, InputProfile::MacbookFunctionKeys]),
            "glove80, macbook-function-keys"
        );
    }

    #[test]
    fn writes_status_responses_larger_than_the_nonblocking_socket_buffer() {
        let (mut server, client) = UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        let response = Response::Ok {
            data: json!({"panes": "x".repeat(32 * 1024)}),
        };

        let writer = thread::spawn(move || -> Result<()> {
            configure_client_stream(&server)?;
            writeln!(server, "{}", serde_json::to_string(&response)?)?;
            Ok(())
        });
        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();

        writer.join().unwrap().unwrap();
        assert!(line.len() > 8192);
        assert!(matches!(
            serde_json::from_str::<Response>(&line).unwrap(),
            Response::Ok { .. }
        ));
    }
}
