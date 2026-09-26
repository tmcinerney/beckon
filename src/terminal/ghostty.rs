//! Ghostty backend: enumerate and focus terminal surfaces through Ghostty's
//! scripting dictionary.
//!
//! All `osascript` usage in Beckon is confined to this module. The programs are
//! JavaScript for Automation (`osascript -l JavaScript`) because it produces
//! escape-safe JSON for the surface list without hand-rolled string building.
//! Focusing a surface by its stable UUID raises its window, selects its tab,
//! and focuses the surface as one dictionary command; the workspace-manager
//! crossing observed in testing comes from that activation.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::{Surface, SurfaceHandle, TerminalBackend};

pub const BACKEND_ID: &str = "ghostty-applescript";

/// Runs a JXA program and returns its stdout. Injectable so surface parsing and
/// failure mapping are unit-testable without a live terminal.
pub trait ScriptRunner {
    fn run(&self, script: &str) -> Result<String>;
}

/// The real runner. The program arrives on stdin, so no quoting, argument
/// length, or shell expansion concerns apply.
pub struct OsascriptRunner;

impl ScriptRunner for OsascriptRunner {
    fn run(&self, script: &str) -> Result<String> {
        let mut child = Command::new("osascript")
            .args(["-l", "JavaScript"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("run osascript")?;
        child
            .stdin
            .as_mut()
            .context("open osascript stdin")?
            .write_all(script.as_bytes())
            .context("write osascript program")?;
        let output = child.wait_with_output().context("wait for osascript")?;
        if !output.status.success() {
            bail!(
                "osascript failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

pub struct GhosttyAppleScript {
    runner: Box<dyn ScriptRunner>,
}

impl Default for GhosttyAppleScript {
    fn default() -> Self {
        Self {
            runner: Box::new(OsascriptRunner),
        }
    }
}

impl GhosttyAppleScript {
    pub fn with_runner(runner: Box<dyn ScriptRunner>) -> Self {
        Self { runner }
    }
}

const LIST_SCRIPT: &str = r#"
const Ghostty = Application("Ghostty");
const surfaces = [];
Ghostty.windows().forEach(w => w.tabs().forEach(t => t.terminals().forEach(term => {
  surfaces.push({ handle: term.id(), title: term.name(), window: w.id(), tab: t.id() });
})));
JSON.stringify({ ok: true, surfaces });
"#;

/// The handle is embedded as a JSON string literal so it cannot break out of
/// the generated program.
fn focus_script(handle: &str) -> String {
    let literal = serde_json::to_string(handle).expect("serializing a string cannot fail");
    format!(
        r#"
const Ghostty = Application("Ghostty");
let target = null;
Ghostty.windows().forEach(w => w.tabs().forEach(t => t.terminals().forEach(term => {{
  if (term.id() === {literal}) target = term;
}})));
if (!target) throw new Error("surface not found");
target.focus();
JSON.stringify({{ ok: true }});
"#
    )
}

#[derive(Deserialize)]
struct ListResponse {
    ok: bool,
    surfaces: Vec<SurfaceEntry>,
}

#[derive(Deserialize)]
struct SurfaceEntry {
    handle: String,
    title: String,
    window: String,
    tab: String,
}

#[derive(Deserialize)]
struct FocusResponse {
    ok: bool,
}

impl TerminalBackend for GhosttyAppleScript {
    fn id(&self) -> &str {
        BACKEND_ID
    }

    fn list_surfaces(&self) -> Result<Vec<Surface>> {
        let stdout = self.runner.run(LIST_SCRIPT)?;
        let response: ListResponse = serde_json::from_str(stdout.trim())
            .with_context(|| format!("decode Ghostty surface list: {}", stdout.trim()))?;
        if !response.ok {
            bail!("Ghostty refused to list terminal surfaces");
        }
        Ok(response
            .surfaces
            .into_iter()
            .map(|entry| Surface {
                handle: SurfaceHandle::new(entry.handle),
                title: entry.title,
                window: entry.window,
                tab: entry.tab,
            })
            .collect())
    }

    fn focus_surface(&self, handle: &SurfaceHandle) -> Result<()> {
        let stdout = self.runner.run(&focus_script(handle.as_str()))?;
        let response: FocusResponse = serde_json::from_str(stdout.trim())
            .with_context(|| format!("decode Ghostty focus response: {}", stdout.trim()))?;
        if !response.ok {
            bail!("Ghostty refused to focus surface {handle}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    struct FakeRunner {
        response: RefCell<Result<String, String>>,
        scripts: RefCell<Vec<String>>,
    }

    impl FakeRunner {
        fn answering(response: &str) -> Self {
            Self {
                response: RefCell::new(Ok(response.to_string())),
                scripts: RefCell::new(Vec::new()),
            }
        }

        fn failing(message: &str) -> Self {
            Self {
                response: RefCell::new(Err(message.to_string())),
                scripts: RefCell::new(Vec::new()),
            }
        }
    }

    impl ScriptRunner for FakeRunner {
        fn run(&self, script: &str) -> Result<String> {
            self.scripts.borrow_mut().push(script.to_string());
            match &*self.response.borrow() {
                Ok(stdout) => Ok(stdout.clone()),
                Err(message) => bail!("{message}"),
            }
        }
    }

    #[test]
    fn parses_the_surface_list_from_the_script_output() {
        let backend = GhosttyAppleScript::with_runner(Box::new(FakeRunner::answering(
            r#"{"ok":true,"surfaces":[{"handle":"A-1","title":"macbook-pro: System","window":"w1","tab":"t1"}]}"#,
        )));
        let surfaces = backend.list_surfaces().unwrap();
        assert_eq!(surfaces.len(), 1);
        assert_eq!(surfaces[0].handle, SurfaceHandle::new("A-1"));
        assert_eq!(surfaces[0].title, "macbook-pro: System");
    }

    #[test]
    fn reports_malformed_or_refused_listings() {
        let malformed =
            GhosttyAppleScript::with_runner(Box::new(FakeRunner::answering("not json")));
        assert!(malformed.list_surfaces().is_err());

        let refused = GhosttyAppleScript::with_runner(Box::new(FakeRunner::answering(
            r#"{"ok":false,"surfaces":[]}"#,
        )));
        assert!(refused.list_surfaces().is_err());
    }

    #[test]
    fn focus_embeds_the_handle_as_a_json_literal() {
        let runner = Box::new(FakeRunner::answering(r#"{"ok":true}"#));
        let backend = GhosttyAppleScript::with_runner(runner);
        backend
            .focus_surface(&SurfaceHandle::new("weird\"handle"))
            .unwrap();

        // The runner is moved into the backend; a second call proves the
        // quoting path did not fail. Re-run with a fresh runner to inspect the
        // generated program.
        let runner = std::rc::Rc::new(FakeRunner::answering(r#"{"ok":true}"#));
        struct Shared(std::rc::Rc<FakeRunner>);
        impl ScriptRunner for Shared {
            fn run(&self, script: &str) -> Result<String> {
                self.0.run(script)
            }
        }
        let backend = GhosttyAppleScript::with_runner(Box::new(Shared(runner.clone())));
        backend
            .focus_surface(&SurfaceHandle::new("weird\"handle"))
            .unwrap();
        let scripts = runner.scripts.borrow();
        assert!(
            scripts[0].contains(r#"term.id() === "weird\"handle""#),
            "{}",
            scripts[0]
        );
    }

    #[test]
    fn maps_runner_failures_to_errors() {
        let backend = GhosttyAppleScript::with_runner(Box::new(FakeRunner::failing(
            "osascript failed: execution error: Error: surface not found (-2700)",
        )));
        let error = backend
            .focus_surface(&SurfaceHandle::new("gone"))
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("surface not found"),
            "{error:#}"
        );
    }
}
