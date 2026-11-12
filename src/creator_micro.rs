//! Built-in Work Louder Creator Micro 2 USB display adapter.
//!
//! The pad is addressed by **AG slot id**, not by matrix position. Per-slot
//! state goes through `v.oai.thstatus` as overrides above the global `keys`
//! zone; the ring is the `ambient` zone of `v.oai.rgbcfg`. Both are
//! fire-and-forget writes so a `v.oai.hid` key notification is never swallowed
//! by a blocking reply read.
//!
//! AIDEV-NOTE: ChatGPT/Codex is a second, layer-agnostic HID host with exactly
//! six slots (ids 0-5) and no concept of layers. The default slot range starts
//! at 6 so the two integrations never write the same override. Re-assert the
//! global `keys` zone on every publish: ChatGPT's inactivity write can darken
//! the seven non-indicator keys, which sit on the base rather than on a slot.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use hidapi::{HidApi, HidDevice};
use serde_json::{Value, json};

use crate::{
    config::CreatorMicroConfig,
    display::{DisplaySink, PublishOutcome},
    render::{Motion, RenderPlan, Rgb},
};

const VENDOR_ID: u16 = 0x303A;
const REPORT_ID: u8 = 0x06;
const CHANNEL_RPC: u8 = 0x02;
const MAX_CHUNK: usize = 61;
const REPORT_SIZE: usize = 64;

/// Firmware effect ids from `v.oai.*`.
const EFFECT_OFF: u8 = 0;
const EFFECT_SOLID: u8 = 1;
const EFFECT_SNAKE: u8 = 2;
const EFFECT_BREATH: u8 = 4;

pub fn rgb_u32(color: Rgb) -> u32 {
    ((color.red as u32) << 16) | ((color.green as u32) << 8) | color.blue as u32
}

fn effect(motion: Motion) -> u8 {
    match motion {
        Motion::Steady => EFFECT_SOLID,
        Motion::Breathe | Motion::Pulse => EFFECT_BREATH,
        Motion::Flicker => EFFECT_SNAKE,
    }
}

/// One `v.oai.hid` notification, already decoded. `key` is the firmware's
/// keycode string; AG keys parse to a slot id, ordinary macro keys do not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidEvent {
    pub key: String,
    pub pressed: bool,
    /// Firmware's `ag` field: the AG slot index, when the key has one.
    pub agent: Option<u8>,
}

impl HidEvent {
    pub fn slot(&self) -> Option<u8> {
        self.agent.or_else(|| {
            self.key
                .strip_prefix("AG")
                .and_then(|suffix| suffix.parse::<u8>().ok())
        })
    }
}

pub struct Pad {
    dev: HidDevice,
    next_id: i64,
    acc: String,
}

impl Pad {
    /// The vendor collection is not the first USB interface, so try every
    /// interface the vendor exposes and keep the one that answers RPC.
    pub fn open() -> Result<Self> {
        let api = HidApi::new().context("hidapi init")?;
        // AIDEV-NOTE: hidapi defaults to an exclusive (seizing) open on macOS.
        // Non-exclusive is required so Input, ChatGPT and Beckon coexist, and
        // requires macOS Input Monitoring for the calling application.
        api.set_open_exclusive(false);
        let candidates = api
            .device_list()
            .filter(|info| info.vendor_id() == VENDOR_ID)
            .map(|info| (info.product_id(), info.path().to_owned()))
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            bail!("no Work Louder device on the HID bus (vendor 0x303A)");
        }

        let mut last = String::new();
        for (pid, path) in candidates {
            let dev = match api.open_path(&path) {
                Ok(dev) => dev,
                Err(err) => {
                    last = format!("{pid:#06x}: open failed: {err}");
                    continue;
                }
            };
            let mut pad = Pad {
                dev,
                next_id: 1,
                acc: String::new(),
            };
            match pad.call("sys.version", json!(null)) {
                Ok(_) => return Ok(pad),
                Err(err) => last = format!("{pid:#06x}: {err:#}"),
            }
        }
        bail!(
            "found the device but no interface answered RPC ({last}). \
             Grant Input Monitoring to the terminal running beckon."
        )
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.send(method, params)?;
        // AIDEV-NOTE: open() probes several vendor interfaces and most never
        // answer RPC. A 5s deadline per candidate made attaching take ~10s.
        // The RPC-capable interface answers in milliseconds.
        let deadline = Instant::now() + Duration::from_millis(700);
        loop {
            if Instant::now() >= deadline {
                bail!("timed out waiting for {method}");
            }
            let Some(value) = self.take_frame()? else {
                continue;
            };
            if value.get("id").and_then(Value::as_i64) != Some(id) {
                continue;
            }
            if let Some(err) = value.get("error") {
                bail!("device returned error: {err}");
            }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Fire-and-forget lighting write. Never waits for a reply.
    pub fn send_only(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(method, params)
    }

    fn send(&mut self, method: &str, params: Value) -> Result<()> {
        let id = self.next_id;
        self.next_id = (self.next_id % 998) + 1;
        let message = json!({ "method": method, "id": id, "params": params });
        let bytes = serde_json::to_vec(&message)?;
        let mut offset = 0;
        while offset < bytes.len() {
            let n = MAX_CHUNK.min(bytes.len() - offset);
            let mut report = vec![0u8; REPORT_SIZE];
            report[0] = REPORT_ID;
            report[1] = CHANNEL_RPC;
            report[2] = n as u8;
            report[3..3 + n].copy_from_slice(&bytes[offset..offset + n]);
            self.dev
                .write(&report)
                .context("IOHIDDeviceSetReport failed")?;
            offset += n;
        }
        Ok(())
    }

    /// Read one HID report and return the next complete JSON frame, if any.
    fn take_frame(&mut self) -> Result<Option<Value>> {
        let mut buf = vec![0u8; REPORT_SIZE];
        let n = self
            .dev
            .read_timeout(&mut buf, 250)
            .context("IOHIDDeviceGetReport failed")?;
        if n == 0 {
            return Ok(None);
        }
        let Some(text) = report_text(&buf[..n]) else {
            return Ok(None);
        };
        self.acc.push_str(text);
        Ok(take_json(&mut self.acc))
    }

    /// Block briefly for a key notification. Returns `None` on timeout.
    pub fn poll_event(&mut self) -> Result<Option<HidEvent>> {
        let mut buf = vec![0u8; REPORT_SIZE];
        let n = self
            .dev
            .read_timeout(&mut buf, 200)
            .context("IOHIDDeviceGetReport failed")?;
        if n == 0 {
            return Ok(None);
        }
        let Some(text) = report_text(&buf[..n]) else {
            return Ok(None);
        };
        self.acc.push_str(text);
        let mut found = None;
        while let Some(value) = take_json(&mut self.acc) {
            if value.get("m").and_then(Value::as_str) != Some("v.oai.hid") {
                continue;
            }
            let params = value.get("p");
            let key = params
                .and_then(|p| p.get("k"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if key.is_empty() {
                continue;
            }
            let pressed = params.and_then(|p| p.get("act")).and_then(Value::as_i64) == Some(1);
            let agent = params
                .and_then(|p| p.get("ag"))
                .and_then(Value::as_u64)
                .map(|value| value as u8);
            found = Some(HidEvent {
                key: key.to_string(),
                pressed,
                agent,
            });
        }
        Ok(found)
    }
}

fn report_text(report: &[u8]) -> Option<&str> {
    let body = if report.first() == Some(&REPORT_ID) {
        &report[1..]
    } else {
        report
    };
    if body.len() < 2 || body[0] != CHANNEL_RPC {
        return None;
    }
    let len = body[1] as usize;
    if len == 0 || body.len() < 2 + len {
        return None;
    }
    std::str::from_utf8(&body[2..2 + len]).ok()
}

/// Pull the first balanced JSON object out of an accumulation buffer.
fn take_json(acc: &mut String) -> Option<Value> {
    let start = acc.find('{')?;
    let bytes = acc.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for index in start..bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                if depth == 0 {
                    let text = acc[start..=index].to_string();
                    acc.drain(..=index);
                    return serde_json::from_str(&text).ok();
                }
            }
            _ => {}
        }
    }
    None
}

/// One complete lighting frame: global base + ring, then six slot overrides.
pub fn write_plan(pad: &mut Pad, plan: &RenderPlan, config: &CreatorMicroConfig) -> Result<()> {
    // The ring reflects the worst state across the bound slots, so it agrees
    // with every other display sink instead of inventing its own priority.
    let (ring_color, ring_brightness, ring_motion) = match plan.worst_state() {
        Some(state) => {
            let treatment = plan.treatments.for_state(state);
            // State normally drives urgency, not identity: the Rex layer keeps
            // its hot pink ring. Opting in lets the ring adopt the worst
            // state's theme colour instead.
            let color = if config.ring_follows_state_color {
                treatment.color
            } else {
                config.ring_color
            };
            (
                color,
                treatment.brightness.max(config.ring_brightness),
                if state == crate::render::AgentState::Blocked {
                    Motion::Pulse
                } else {
                    config.ring_motion
                },
            )
        }
        None => (
            config.ring_color,
            config.ring_brightness,
            config.ring_motion,
        ),
    };

    pad.send_only(
        "v.oai.rgbcfg",
        json!({
            "keys": {
                "e": EFFECT_SOLID,
                "b": config.base_brightness,
                "s": 0.5,
                "m": 1,
                "c": rgb_u32(config.base_color),
            },
            "ambient": {
                "e": effect(ring_motion),
                "b": ring_brightness,
                "s": 0.3,
                "m": 0.35,
                "c": rgb_u32(ring_color),
            },
        }),
    )?;

    let threads = config
        .slots
        .iter()
        .zip(plan.keys.iter())
        .map(|(slot, key)| match key.state {
            None => json!({ "id": slot, "c": 0, "b": 0.0, "e": EFFECT_OFF, "s": 0.5 }),
            Some(_) => json!({
                "id": slot,
                "c": rgb_u32(key.color),
                "b": key.brightness,
                "e": effect(key.motion),
                "s": 0.5,
            }),
        })
        .collect::<Vec<_>>();
    pad.send_only("v.oai.thstatus", Value::Array(threads))
}

/// Turn every indicator and the ring off. Used by `creator-micro off`.
pub fn clear(pad: &mut Pad, config: &CreatorMicroConfig) -> Result<()> {
    pad.send_only(
        "v.oai.rgbcfg",
        json!({
            "keys": { "e": EFFECT_OFF, "b": 0.0, "s": 0.5, "m": 1, "c": 0 },
            "ambient": { "e": EFFECT_OFF, "b": 0.0, "s": 0.3, "m": 0.35, "c": 0 },
        }),
    )?;
    let threads = config
        .slots
        .iter()
        .map(|slot| json!({ "id": slot, "c": 0, "b": 0.0, "e": EFFECT_OFF, "s": 0.5 }))
        .collect::<Vec<_>>();
    pad.send_only("v.oai.thstatus", Value::Array(threads))
}

/// Lazily attached pad. A missing device is `Unavailable`, never a startup or
/// navigation failure, and reconnects on a later tick.
pub struct CreatorMicroUsbDisplay {
    config: CreatorMicroConfig,
    pad: Option<Pad>,
    last_plan: Option<RenderPlan>,
    last_write: Option<Instant>,
    refresh_every: Duration,
}

impl CreatorMicroUsbDisplay {
    pub fn new(config: CreatorMicroConfig) -> Self {
        Self {
            config,
            pad: None,
            last_plan: None,
            last_write: None,
            refresh_every: Duration::from_secs(2),
        }
    }

    /// The firmware drops AG slot overrides when the writing HID host closes
    /// its handle, and ChatGPT periodically rewrites the global `keys` zone.
    /// Re-assert a full frame on a slow tick even when the plan is unchanged.
    fn needs_write(&self, plan: &RenderPlan, now: Instant) -> bool {
        if self.last_plan.as_ref() != Some(plan) {
            return true;
        }
        self.last_write
            .is_none_or(|last| now.duration_since(last) >= self.refresh_every)
    }
}

impl DisplaySink for CreatorMicroUsbDisplay {
    fn id(&self) -> &str {
        "creator-micro-usb"
    }

    fn publish(&mut self, plan: &RenderPlan) -> Result<PublishOutcome> {
        if !self.needs_write(plan, Instant::now()) {
            return Ok(PublishOutcome::Unchanged);
        }
        if self.pad.is_none() {
            match Pad::open() {
                Ok(pad) => self.pad = Some(pad),
                Err(_) => return Ok(PublishOutcome::Unavailable),
            }
        }
        // A failed write usually means the pad was unplugged. Drop the handle
        // so the next tick retries from a fresh open.
        let result = write_plan(
            self.pad.as_mut().expect("pad opened above"),
            plan,
            &self.config,
        );
        match result {
            Ok(()) => {
                self.last_plan = Some(plan.clone());
                self.last_write = Some(Instant::now());
                Ok(PublishOutcome::Updated)
            }
            Err(error) => {
                self.pad = None;
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_packs_as_a_single_firmware_integer() {
        assert_eq!(
            rgb_u32(Rgb {
                red: 0xFF,
                green: 0x49,
                blue: 0x75
            }),
            16_730_485
        );
        assert_eq!(
            rgb_u32(Rgb {
                red: 0xB3,
                green: 0x66,
                blue: 0xFF
            }),
            11_757_311
        );
    }

    #[test]
    fn report_text_ignores_non_rpc_frames() {
        assert!(report_text(&[0x06, 0x01, 3, b'a', b'b', b'c']).is_none());
        assert!(report_text(&[0x06, 0x02, 0]).is_none());
    }

    #[test]
    fn hid_events_keep_non_ag_keycodes_for_the_mic_key() {
        let ag = HidEvent {
            key: "AG06".into(),
            pressed: true,
            agent: None,
        };
        assert_eq!(ag.slot(), Some(6));
        let by_field = HidEvent {
            key: "KV_OAI_ACT12".into(),
            pressed: true,
            agent: Some(12),
        };
        assert_eq!(by_field.slot(), Some(12));
        let macro_key = HidEvent {
            key: "KA_M1".into(),
            pressed: true,
            agent: None,
        };
        assert_eq!(macro_key.slot(), None);
        assert_eq!(macro_key.key, "KA_M1");
    }

    #[test]
    fn take_json_survives_a_chunk_boundary() {
        let mut acc = String::from("{\"m\":\"v.oai.hid\",");
        let first = take_json(&mut acc);
        assert!(first.is_none());
        acc.push_str("\"p\":{\"k\":\"AG06\",\"act\":1}}");
        let value = take_json(&mut acc).unwrap();
        assert_eq!(value["p"]["k"], "AG06");
        assert!(acc.is_empty());
    }
}
