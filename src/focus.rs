use std::process::Command;

use anyhow::{Context, Result, bail};
use tracing::debug;

use crate::config::FocusConfig;
use crate::core::PaneRef;
use crate::terminal::SurfaceHandle;

/// Everything the user's focus command may need about the navigation target.
///
/// The command is the machine-specific seam: a window manager integration, an
/// extra application, or a layout Beckon cannot model. It receives the logical
/// key, the exact pane, and the adopted terminal surface handle when one was
/// raised, so it can compose with the terminal backend instead of guessing.
pub struct FocusContext<'a> {
    pub key: &'a str,
    pub pane: &'a PaneRef,
    pub terminal_handle: Option<&'a SurfaceHandle>,
}

/// User-configured focus integration. Beckon intentionally has no window
/// manager dependency; OmniWM, Aerospace, and plain macOS use this same port.
pub trait FocusAdapter {
    fn focus_terminal(&self, context: &FocusContext<'_>) -> Result<()>;
}

pub struct CommandFocus<'a> {
    config: &'a FocusConfig,
}

impl<'a> CommandFocus<'a> {
    pub fn new(config: &'a FocusConfig) -> Self {
        Self { config }
    }
}

impl FocusAdapter for CommandFocus<'_> {
    fn focus_terminal(&self, context: &FocusContext<'_>) -> Result<()> {
        let Some(command) = self.config.command.as_deref() else {
            return Ok(());
        };
        let (program, arguments) = command
            .split_first()
            .expect("configuration rejects an empty focus command");
        debug!(
            program,
            argument_count = arguments.len(),
            "run terminal focus integration"
        );
        let mut process = Command::new(program);
        process
            .args(arguments)
            .env("BECKON_KEY", context.key)
            .env("BECKON_HERDR_SESSION", &context.pane.session)
            .env("BECKON_PANE_ID", &context.pane.pane_id);
        if let Some(handle) = context.terminal_handle {
            process.env("BECKON_TERMINAL_HANDLE", handle.as_str());
        }
        let status = process
            .status()
            .with_context(|| format!("run focus command {program}"))?;
        if !status.success() {
            bail!("focus command {program} exited with {status}");
        }
        debug!(program, "terminal focus integration succeeded");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context<'a>(pane: &'a PaneRef, handle: Option<&'a SurfaceHandle>) -> FocusContext<'a> {
        FocusContext {
            key: "f3",
            pane,
            terminal_handle: handle,
        }
    }

    fn no_focus_command() -> FocusConfig {
        FocusConfig { command: None }
    }

    #[test]
    fn exports_the_navigation_context_to_the_command() {
        let pane = PaneRef::new("agent-workspace", "w6:p9");
        let handle = SurfaceHandle::new("HANDLE-1");
        let config: FocusConfig = toml::from_str(
            r#"command = ["/bin/sh", "-c", "test \"$BECKON_KEY\" = f3 && test \"$BECKON_HERDR_SESSION\" = agent-workspace && test \"$BECKON_PANE_ID\" = w6:p9 && test \"$BECKON_TERMINAL_HANDLE\" = HANDLE-1"]
"#,
        )
        .unwrap();

        CommandFocus::new(&config)
            .focus_terminal(&context(&pane, Some(&handle)))
            .unwrap();
    }

    #[test]
    fn omits_the_handle_environment_when_none_was_raised() {
        let pane = PaneRef::new("default", "w1:p1");
        let config: FocusConfig = toml::from_str(
            r#"command = ["/bin/sh", "-c", "test -z \"${BECKON_TERMINAL_HANDLE+set}\""]
"#,
        )
        .unwrap();

        CommandFocus::new(&config)
            .focus_terminal(&context(&pane, None))
            .unwrap();
    }

    #[test]
    fn a_failing_command_is_a_navigation_failure() {
        let pane = PaneRef::new("default", "w1:p1");
        let config: FocusConfig = toml::from_str(
            r#"command = ["/bin/sh", "-c", "exit 7"]
"#,
        )
        .unwrap();

        let error = CommandFocus::new(&config)
            .focus_terminal(&context(&pane, None))
            .unwrap_err();
        assert!(format!("{error:#}").contains("exited with"), "{error:#}");
    }

    #[test]
    fn an_absent_command_is_a_quiet_no_op() {
        let pane = PaneRef::new("default", "w1:p1");
        let config = no_focus_command();
        CommandFocus::new(&config)
            .focus_terminal(&context(&pane, None))
            .unwrap();
    }
}
