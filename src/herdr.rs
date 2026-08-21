use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::core::{Pane, PaneDirectory};

const SOURCE: &str = "beckond";

pub struct HerdrCli;

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
