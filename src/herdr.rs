use std::{
    env,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::Command,
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    core::{Pane, PaneDirectory},
    pane_cache::PaneEvent,
};

const SOURCE: &str = "beckond";

pub struct HerdrCli;

/// Live read directory plus the existing, verified mutation commands.
///
/// Herdr's event socket is ideal for long-lived state. The CLI remains the
/// deliberately narrow command adapter for the already-proven metadata and
/// agent-focus operations; importantly, no binding operation polls it.
pub struct LivePaneDirectory {
    cache: crate::pane_cache::PaneCache,
    commands: HerdrCli,
}

impl LivePaneDirectory {
    pub fn start() -> Result<Self> {
        Ok(Self {
            cache: crate::pane_cache::PaneCache::start(HerdrSocket::from_environment())?,
            commands: HerdrCli,
        })
    }

    pub fn cache(&self) -> &crate::pane_cache::PaneCache {
        &self.cache
    }
}

impl PaneDirectory for LivePaneDirectory {
    fn panes(&self) -> Result<Vec<Pane>> {
        Ok(self.cache.panes())
    }

    fn write_fkey(&self, pane_id: &str, key: Option<&str>) -> Result<()> {
        self.commands.write_fkey(pane_id, key)
    }

    fn focus_agent(&self, pane_id: &str) -> Result<()> {
        self.commands.focus_agent(pane_id)
    }
}

/// Raw Herdr's Unix-domain socket transport. This is deliberately a narrow
/// adapter: consumers get panes and normalized pane events, never wire JSON.
#[derive(Clone, Debug)]
pub struct HerdrSocket {
    path: PathBuf,
}

impl HerdrSocket {
    pub fn from_environment() -> Self {
        Self {
            // Beckon owns this override rather than assuming an undocumented
            // Herdr environment variable. It also makes socket tests hermetic.
            path: env::var_os("BECKON_HERDR_SOCKET")
                .map(PathBuf::from)
                .unwrap_or_else(default_socket_path),
        }
    }

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
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("herdr/herdr.sock")
}

impl PaneDirectory for HerdrCli {
    fn panes(&self) -> Result<Vec<Pane>> {
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

    fn write_fkey(&self, pane_id: &str, key: Option<&str>) -> Result<()> {
        let mut command = Command::new("herdr");
        command.args(["pane", "report-metadata", pane_id, "--source", SOURCE]);
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

    fn focus_agent(&self, pane_id: &str) -> Result<()> {
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
            .map(PaneEvent::Upsert)
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
    fn decodes_updated_event_with_full_pane() {
        let event = decode_pane_event(
            r#"{"event":"pane_updated","data":{"type":"pane_updated","pane":{"pane_id":"w:p","agent_status":"working"}}}"#,
        )
        .unwrap();
        assert!(
            matches!(event, Some(PaneEvent::Upsert(Pane { pane_id, agent_status, .. })) if pane_id == "w:p" && agent_status == "working")
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
}
