use std::{
    collections::BTreeSet,
    env, fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::debug;

use crate::{
    core::{DEFAULT_SESSION, Pane, PaneDirectory, PaneRef, PresentationTokenWrite},
    pane_cache::PaneEvent,
};

const SOURCE: &str = "beckond";

/// One discoverable Herdr server instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HerdrSession {
    pub name: String,
    pub socket_path: PathBuf,
}

/// Discovers the Herdr sessions Beckon should manage.
///
/// The default session's socket is `BECKON_HERDR_SOCKET` (a test seam), then
/// `[herdr] socket`, then `~/.config/herdr/herdr.sock`. Named sessions come
/// from `~/.config/herdr/sessions/<name>/herdr.sock` (root overridable with
/// `BECKON_HERDR_SESSIONS_DIR` for hermetic tests). Stopped sessions leave
/// their directories and sockets behind, so every candidate is liveness-checked
/// and dead ones are skipped. `allowed` is the `[herdr] sessions` allowlist;
/// `None` manages every live session.
pub fn discover_sessions(
    configured_socket: Option<&Path>,
    allowed: Option<&[String]>,
) -> Vec<HerdrSession> {
    if let Some(pinned) = env::var_os("BECKON_HERDR_SOCKET") {
        // Hermetic tests pin exactly one socket and must not observe the
        // machine's real session directory.
        return vec![HerdrSession {
            name: DEFAULT_SESSION.into(),
            socket_path: PathBuf::from(pinned),
        }];
    }

    let candidates = collect_session_candidates(
        configured_socket
            .map(Path::to_path_buf)
            .unwrap_or_else(default_socket_path),
        &sessions_dir_path(),
        allowed,
    );
    candidates
        .into_iter()
        .filter(|session| {
            let alive = socket_is_live(&session.socket_path);
            if !alive && allowed.is_some() {
                // An explicit allowlist names sessions deliberately; say why a
                // configured session is missing instead of ignoring it.
                eprintln!(
                    "beckon: session {} is configured but its socket {} is not reachable",
                    session.name,
                    session.socket_path.display()
                );
            }
            alive
        })
        .collect()
}

/// Candidate sessions in deterministic order (default first, then named
/// sessions sorted by name) without any liveness filtering.
fn collect_session_candidates(
    default_socket: PathBuf,
    sessions_dir: &Path,
    allowed: Option<&[String]>,
) -> Vec<HerdrSession> {
    let permits =
        |name: &str| allowed.is_none_or(|names| names.iter().any(|allowed| allowed == name));

    let mut sessions = Vec::new();
    if permits(DEFAULT_SESSION) {
        sessions.push(HerdrSession {
            name: DEFAULT_SESSION.into(),
            socket_path: default_socket,
        });
    }

    let mut named = Vec::new();
    if allowed.is_none_or(|names| names.iter().any(|name| name != DEFAULT_SESSION))
        && let Ok(entries) = fs::read_dir(sessions_dir)
    {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // The default session lives at the root socket path, never under
            // the sessions directory.
            if name == DEFAULT_SESSION || !permits(&name) {
                continue;
            }
            let socket_path = entry.path().join("herdr.sock");
            if socket_path.exists() {
                named.push(HerdrSession { name, socket_path });
            }
        }
    }
    named.sort_by(|left, right| left.name.cmp(&right.name));
    sessions.extend(named);
    sessions
}

fn socket_is_live(path: &Path) -> bool {
    UnixStream::connect(path).is_ok()
}

/// Live read directory plus the existing, verified mutation commands for one
/// session.
///
/// Herdr's event socket is ideal for long-lived state. The CLI remains the
/// deliberately narrow command adapter for the already-proven metadata
/// operations; targeting and pane focus stay on the socket.
pub struct LivePaneDirectory {
    session: HerdrSession,
    cache: crate::pane_cache::PaneCache,
    commands: HerdrCli,
}

impl LivePaneDirectory {
    pub fn start(session: HerdrSession, cli: &str) -> Result<Self> {
        let cache = crate::pane_cache::PaneCache::start(
            HerdrSocket::new(session.socket_path.clone()),
            &session.name,
        )?;
        let commands = HerdrCli::for_session(cli, &session);
        Ok(Self {
            session,
            cache,
            commands,
        })
    }

    pub fn session(&self) -> &HerdrSession {
        &self.session
    }

    pub fn cache(&self) -> &crate::pane_cache::PaneCache {
        &self.cache
    }
}

impl PaneDirectory for LivePaneDirectory {
    fn panes(&self) -> Result<Vec<Pane>> {
        Ok(self.cache.panes())
    }

    fn observed_sessions(&self) -> Result<BTreeSet<String>> {
        Ok(BTreeSet::from([self.session.name.clone()]))
    }

    fn write_fkey(&self, pane: &PaneRef, key: Option<&str>) -> Result<()> {
        self.commands.write_fkey(pane, key)
    }

    fn write_presentation_tokens(
        &self,
        pane: &PaneRef,
        binding: &str,
    ) -> Result<PresentationTokenWrite> {
        self.commands.write_presentation_tokens(pane, binding)
    }

    fn focus_pane(&self, pane: &PaneRef) -> Result<()> {
        self.commands.focus_pane(pane)
    }

    fn send_keys(&self, pane: &PaneRef, keys: &[&str]) -> Result<()> {
        self.commands.send_keys(pane, keys)
    }
}

/// Raw Herdr's Unix-domain socket transport. This is deliberately a narrow
/// adapter: consumers get panes and normalized pane events, never wire JSON.
#[derive(Clone, Debug)]
pub struct HerdrSocket {
    path: PathBuf,
}

impl HerdrSocket {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn panes(&self) -> Result<Vec<Pane>> {
        let response = self.request(json!({
            "id": "beckond:pane-list",
            "method": "pane.list",
            "params": {}
        }))?;
        serde_json::from_value::<PaneListResponse>(response)
            .map(|response| response.result.panes)
            .context("decode Herdr pane.list response")
    }

    /// Subscribe once per socket connection. Returning from the callback ends
    /// the monitor only when Herdr closes the stream or returns malformed data.
    pub fn monitor(&self, mut apply: impl FnMut(PaneEvent)) -> Result<()> {
        let mut stream = self.connect()?;
        let subscription = json!({
            "id": "beckond:pane-events",
            "method": "events.subscribe",
            "params": {"subscriptions": [
                {"type": "pane.created"},
                {"type": "pane.updated"},
                {"type": "pane.closed"},
                {"type": "pane.exited"}
            ]}
        });
        writeln!(stream, "{}", serde_json::to_string(&subscription)?)?;
        stream.flush()?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                bail!("Herdr closed event stream");
            }
            if let Some(event) = decode_pane_event(line.trim())? {
                apply(event);
            }
        }
    }

    /// Focus is a pane operation, whether that pane hosts a recognized agent
    /// or an ordinary shell. Agent focus rejects the latter.
    pub fn focus_pane(&self, pane_id: &str) -> Result<()> {
        debug!(pane_id, socket = %self.path.display(), "send Herdr pane.focus request");
        let response = self.request(json!({
            "id": "beckond:pane-focus",
            "method": "pane.focus",
            "params": {"pane_id": pane_id}
        }))?;
        if let Some(error) = response.get("error") {
            bail!("Herdr pane focus failed: {error}");
        }
        debug!(pane_id, "received successful Herdr pane.focus response");
        Ok(())
    }

    /// Send validated logical key names to one exact pane. This is used only
    /// by explicitly enabled actions; ordinary Beckon navigation never sends
    /// input to agents or shells.
    pub fn send_keys(&self, pane_id: &str, keys: &[&str]) -> Result<()> {
        let response = self.request(json!({
            "id": "beckond:pane-send-keys",
            "method": "pane.send_keys",
            "params": {"pane_id": pane_id, "keys": keys}
        }))?;
        if let Some(error) = response.get("error") {
            bail!("Herdr pane send_keys failed: {error}");
        }
        Ok(())
    }

    fn request(&self, request: Value) -> Result<Value> {
        let mut stream = self.connect()?;
        writeln!(stream, "{}", serde_json::to_string(&request)?)?;
        stream.flush()?;
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response)?;
        if response.trim().is_empty() {
            bail!("Herdr closed request stream without a response");
        }
        serde_json::from_str(&response).context("decode Herdr response")
    }

    fn connect(&self) -> Result<UnixStream> {
        UnixStream::connect(&self.path)
            .with_context(|| format!("connect to Herdr socket {}", self.path.display()))
    }
}

fn default_socket_path() -> PathBuf {
    herdr_config_dir().join("herdr/herdr.sock")
}

fn sessions_dir_path() -> PathBuf {
    env::var_os("BECKON_HERDR_SESSIONS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| herdr_config_dir().join("herdr/sessions"))
}

fn herdr_config_dir() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
}

/// The `herdr` CLI adapter for one session.
///
/// Metadata and pane listing use the CLI because those commands are already
/// proven there. This mirrors the original adapter, which used the socket for
/// focus and key delivery even when constructed as a CLI directory.
#[derive(Clone, Debug)]
pub struct HerdrCli {
    program: String,
    session: String,
    socket_path: PathBuf,
}

impl HerdrCli {
    pub fn for_session(program: &str, session: &HerdrSession) -> Self {
        Self {
            program: program.into(),
            session: session.name.clone(),
            socket_path: session.socket_path.clone(),
        }
    }

    /// The global `--session` flag selects a named server. The default session
    /// keeps its historical invocation exactly: no flag.
    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(session_arguments(&self.session));
        command
    }

    fn socket(&self) -> HerdrSocket {
        HerdrSocket::new(self.socket_path.clone())
    }

    fn ensure_same_session(&self, pane: &PaneRef) -> Result<()> {
        if pane.session != self.session {
            bail!(
                "pane {} belongs to session {}, not {}",
                pane.pane_id,
                pane.session,
                self.session
            );
        }
        Ok(())
    }
}

/// Argument prefix for a session-addressed CLI invocation.
fn session_arguments(session: &str) -> Vec<String> {
    if session == DEFAULT_SESSION {
        Vec::new()
    } else {
        vec!["--session".to_string(), session.to_string()]
    }
}

impl PaneDirectory for HerdrCli {
    fn panes(&self) -> Result<Vec<Pane>> {
        let output = self
            .command()
            .args(["pane", "list"])
            .output()
            .context("run herdr pane list")?;
        if !output.status.success() {
            bail!(
                "herdr pane list failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let mut panes = serde_json::from_slice::<PaneListResponse>(&output.stdout)?
            .result
            .panes;
        for pane in &mut panes {
            pane.session.clone_from(&self.session);
        }
        Ok(panes)
    }

    fn observed_sessions(&self) -> Result<BTreeSet<String>> {
        Ok(BTreeSet::from([self.session.clone()]))
    }

    fn write_fkey(&self, pane: &PaneRef, key: Option<&str>) -> Result<()> {
        self.ensure_same_session(pane)?;
        let mut command = self.command();
        command.args(["pane", "report-metadata", &pane.pane_id, "--source", SOURCE]);
        match key {
            Some(key) => command.arg("--token").arg(format!("fkey={key}")),
            None => command.arg("--clear-token").arg("fkey"),
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

    fn write_presentation_tokens(
        &self,
        pane: &PaneRef,
        binding: &str,
    ) -> Result<PresentationTokenWrite> {
        self.ensure_same_session(pane)?;
        let output = self
            .command()
            .args(["pane", "report-metadata", &pane.pane_id, "--source", SOURCE])
            .arg("--token")
            .arg(format!("beckon_binding={binding}"))
            .arg("--token")
            .arg(format!("beckon_pane_id={}", pane.pane_id))
            .output()
            .context("write Beckon presentation tokens")?;
        if output.status.success() {
            return Ok(PresentationTokenWrite::Written);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if is_pane_not_found(&stderr) {
            return Ok(PresentationTokenWrite::PaneGone);
        }
        bail!("presentation token update failed: {}", stderr.trim());
    }

    fn focus_pane(&self, pane: &PaneRef) -> Result<()> {
        self.ensure_same_session(pane)?;
        self.socket().focus_pane(&pane.pane_id)
    }

    fn send_keys(&self, pane: &PaneRef, keys: &[&str]) -> Result<()> {
        self.ensure_same_session(pane)?;
        self.socket().send_keys(&pane.pane_id, keys)
    }
}

fn is_pane_not_found(stderr: &str) -> bool {
    serde_json::from_str::<Value>(stderr)
        .ok()
        .and_then(|error| {
            error
                .pointer("/error/code")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .as_deref()
        == Some("pane_not_found")
}

#[derive(Debug, Deserialize)]
struct PaneListResponse {
    result: PaneListResult,
}

#[derive(Debug, Deserialize)]
struct PaneListResult {
    panes: Vec<Pane>,
}

#[derive(Debug, Deserialize)]
struct HerdrEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    pane: Option<Pane>,
    #[serde(default)]
    pane_id: Option<String>,
}

/// Accept both the event payload emitted by the event stream and the wrapped
/// `result` form used by some Herdr protocol responses.
fn decode_pane_event(line: &str) -> Result<Option<PaneEvent>> {
    let value: Value = serde_json::from_str(line)?;
    let event: HerdrEvent = serde_json::from_value(
        value
            .get("data")
            .or_else(|| value.get("result"))
            .cloned()
            .unwrap_or(value),
    )?;
    match event.event_type.as_str() {
        "pane_created" | "pane_updated" => event
            .pane
            .map(|pane| PaneEvent::Upsert(Box::new(pane)))
            .context("pane event omitted pane")
            .map(Some),
        "pane_closed" | "pane_exited" => event
            .pane_id
            .or_else(|| event.pane.map(|pane| pane.pane_id))
            .map(PaneEvent::Remove)
            .context("pane removal event omitted pane_id")
            .map(Some),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
        thread,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn unique_temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "beckon-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn session_arguments_only_add_the_flag_for_named_sessions() {
        assert!(session_arguments(DEFAULT_SESSION).is_empty());
        assert_eq!(
            session_arguments("agent-workspace"),
            ["--session", "agent-workspace"]
        );
    }

    #[test]
    fn collects_candidates_with_default_first_and_named_sessions_sorted() {
        let root = unique_temp_path("sessions-candidates");
        let sessions_dir = root.join("sessions");
        for name in ["beta", "alpha"] {
            fs::create_dir_all(sessions_dir.join(name)).unwrap();
            fs::write(sessions_dir.join(name).join("herdr.sock"), b"").unwrap();
        }
        let default_socket = root.join("herdr.sock");

        let all = collect_session_candidates(default_socket.clone(), &sessions_dir, None);
        let names = all
            .iter()
            .map(|session| session.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["default", "alpha", "beta"]);
        assert_eq!(all[0].socket_path, default_socket);

        let only = ["default".to_string(), "alpha".to_string()];
        let filtered = collect_session_candidates(default_socket, &sessions_dir, Some(&only));
        let names = filtered
            .iter()
            .map(|session| session.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["default", "alpha"]);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn liveness_check_rejects_dead_sockets() {
        let path = unique_temp_path("liveness");
        let listener = UnixListener::bind(&path).unwrap();
        assert!(socket_is_live(&path));
        drop(listener);
        fs::remove_file(&path).unwrap();

        fs::write(&path, b"not a socket").unwrap();
        assert!(!socket_is_live(&path));
        assert!(!socket_is_live(&path.with_extension("missing")));
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_pinned_socket_yields_exactly_the_default_session() {
        let pinned = unique_temp_path("pinned");
        // SAFETY: this test is the only writer or reader of the pin outside of
        // production discovery; no other test observes this variable.
        unsafe {
            env::set_var("BECKON_HERDR_SOCKET", &pinned);
        }
        let sessions = discover_sessions(None, None);
        unsafe {
            env::remove_var("BECKON_HERDR_SOCKET");
        }

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, DEFAULT_SESSION);
        assert_eq!(sessions[0].socket_path, pinned);
    }

    #[test]
    fn requests_a_pane_snapshot_over_ndjson() {
        let path = std::env::temp_dir().join(format!(
            "beckon-herdr-test-{}-{}.sock",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let request: Value = serde_json::from_str(&request).unwrap();
            assert_eq!(request["method"], "pane.list");
            assert_eq!(request["params"], json!({}));
            writeln!(
                stream,
                r#"{{"result":{{"panes":[{{"pane_id":"p1","agent_status":"idle"}}]}}}}"#
            )
            .unwrap();
        });

        let panes = HerdrSocket::new(path.clone()).panes().unwrap();
        server.join().unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].pane_id, "p1");
    }

    #[test]
    fn sends_logical_keys_to_an_explicit_pane() {
        let path = std::env::temp_dir().join(format!(
            "beckon-herdr-keys-test-{}-{}.sock",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let request: Value = serde_json::from_str(&request).unwrap();
            assert_eq!(request["method"], "pane.send_keys");
            assert_eq!(
                request["params"],
                json!({"pane_id": "w:p1", "keys": ["enter"]})
            );
            writeln!(stream, r#"{{"result":{{"type":"pane_send_keys"}}}}"#).unwrap();
        });

        HerdrSocket::new(path.clone())
            .send_keys("w:p1", &["enter"])
            .unwrap();
        server.join().unwrap();
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn decodes_updated_event_with_full_pane() {
        let event = decode_pane_event(
            r#"{"event":"pane_updated","data":{"type":"pane_updated","pane":{"pane_id":"w:p","agent_status":"working"}}}"#,
        )
        .unwrap();
        assert!(
            matches!(event, Some(PaneEvent::Upsert(pane)) if pane.pane_id == "w:p" && pane.agent_status == "working")
        );
    }

    #[test]
    fn decodes_wrapped_closed_event() {
        let event = decode_pane_event(
            r#"{"event":"pane_closed","data":{"type":"pane_closed","pane_id":"w:p"}}"#,
        )
        .unwrap();
        assert_eq!(event, Some(PaneEvent::Remove("w:p".into())));
    }

    #[test]
    fn ignores_non_pane_events() {
        assert_eq!(
            decode_pane_event(r#"{"id":"subscription","result":{"type":"subscription_started"}}"#)
                .unwrap(),
            None
        );
    }

    #[test]
    fn recognizes_structured_pane_not_found_errors() {
        assert!(is_pane_not_found(
            r#"{"error":{"code":"pane_not_found","message":"pane w:p no longer exists"}}"#
        ));
        assert!(!is_pane_not_found(
            r#"{"error":{"code":"permission_denied","message":"not allowed"}}"#
        ));
    }
}
