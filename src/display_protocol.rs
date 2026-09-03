//! Versioned wire format for out-of-process display plugins.

use anyhow::Result;
use serde::Serialize;

use crate::render::RenderPlan;

pub const PROTOCOL_NAME: &str = "beckon.display";
pub const PROTOCOL_VERSION: u32 = 1;

/// One newline-delimited JSON message written to a display plugin's stdin.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DisplayMessage<'a> {
    Hello {
        protocol: &'static str,
        version: u32,
        plugin_id: &'a str,
    },
    Render {
        protocol: &'static str,
        version: u32,
        sequence: u64,
        plan: &'a RenderPlan,
    },
}

impl<'a> DisplayMessage<'a> {
    pub fn hello(plugin_id: &'a str) -> Self {
        Self::Hello {
            protocol: PROTOCOL_NAME,
            version: PROTOCOL_VERSION,
            plugin_id,
        }
    }

    pub fn render(sequence: u64, plan: &'a RenderPlan) -> Self {
        Self::Render {
            protocol: PROTOCOL_NAME,
            version: PROTOCOL_VERSION,
            sequence,
            plan,
        }
    }

    pub fn to_ndjson(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::render::DisplayConfig;

    #[test]
    fn hello_identifies_protocol_version_and_plugin() {
        let value = serde_json::to_value(DisplayMessage::hello("status-log")).unwrap();

        assert_eq!(
            value,
            json!({
                "type": "hello",
                "protocol": "beckon.display",
                "version": 1,
                "plugin_id": "status-log"
            })
        );
    }

    #[test]
    fn render_is_a_complete_self_describing_snapshot() {
        let plan = crate::render::render(&DisplayConfig::default(), &[]).unwrap();
        let value = serde_json::to_value(DisplayMessage::render(7, &plan)).unwrap();

        assert_eq!(value["type"], "render");
        assert_eq!(value["protocol"], "beckon.display");
        assert_eq!(value["version"], 1);
        assert_eq!(value["sequence"], 7);
        assert_eq!(value["plan"]["keys"].as_array().unwrap().len(), 10);
        assert_eq!(value["plan"]["keys"][0]["key"], "f1");
        assert!(value["plan"]["treatments"].is_object());
    }

    #[test]
    fn ndjson_messages_end_in_exactly_one_newline() {
        let bytes = DisplayMessage::hello("status-log").to_ndjson().unwrap();

        assert!(bytes.ends_with(b"\n"));
        assert!(!bytes.ends_with(b"\n\n"));
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap();
    }
}
