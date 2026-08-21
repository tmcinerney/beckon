use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::FocusConfig;

/// User-configured focus integration. Beckon intentionally has no window
/// manager dependency; OmniWM, Aerospace, and plain macOS use this same port.
pub trait FocusAdapter {
    fn focus_terminal(&self) -> Result<()>;
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
    fn focus_terminal(&self) -> Result<()> {
        let Some(command) = self.config.command.as_deref() else {
            return Ok(());
        };
        let (program, arguments) = command
            .split_first()
            .expect("configuration rejects an empty focus command");
        let status = Command::new(program)
            .args(arguments)
            .status()
            .with_context(|| format!("run focus command {program}"))?;
        if !status.success() {
            bail!("focus command {program} exited with {status}");
        }
        Ok(())
    }
}
