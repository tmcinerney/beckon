//! Built-in Work Louder Creator Micro 2 HID adapter (USB verified; Bluetooth unverified).
//!
//! The pad is addressed by **AG slot id**, not by matrix position. Per-slot
//! state goes through `v.oai.thstatus` as overrides above the global `keys`
//! zone; the ring is the `ambient` zone of `v.oai.rgbcfg`. Both are
//! fire-and-forget writes so a `v.oai.hid` key notification is never swallowed
//! by a blocking reply read.
//!
//! AIDEV-NOTE: ChatGPT is a second HID host owning ids 0-5. Config can only
//! claim 6-18, and only on explicitly owned layers. No lighting is published
//! on other layers. Re-assert the keys zone on owned layers because the other
//! host can overwrite it independently of our render state.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use hidapi::{HidApi, HidDevice};
use serde_json::{Value, json};
use tracing::{debug, info};

use crate::{
    device::{Address, DeviceConfig, Treatment},
    display::{DeviceEvent, DisplaySink, PublishOutcome},
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

pub struct Pad {
    dev: HidDevice,
    next_id: i64,
    acc: String,
    pending: VecDeque<Value>,
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
            .filter(|info| {
                info.vendor_id() == VENDOR_ID && matches!(info.product_id(), 0x8297 | 0x8298)
            })
            .map(|info| (info.product_id(), info.path().to_owned()))
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            bail!("no Creator Micro 2 on the HID bus (303A:8297 or 303A:8298)");
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
                pending: VecDeque::new(),
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
        self.call_json(method, params)
    }

    /// Public RPC access for the filesystem transport in
    /// [`crate::creator_micro_fs`]. Kept on `Pad` so one HID handle is shared.
    pub fn call_json(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.send(method, params)?;
        // AIDEV-NOTE: open() probes several vendor interfaces and most never
        // answer RPC, so the probe uses a short deadline. Filesystem writes
        // move 3KB chunks and need more room; use a separate per-method budget.
        let budget = if method.starts_with("fs.") {
            Duration::from_secs(5)
        } else {
            Duration::from_millis(700)
        };
        let deadline = Instant::now() + budget;
        loop {
            if Instant::now() >= deadline {
                bail!("timed out waiting for {method}");
            }
            let Some(value) = self.take_frame()? else {
                continue;
            };
            if value.get("id").and_then(Value::as_i64) != Some(id) {
                if value.get("m").is_some() {
                    self.pending.push_back(value);
                }
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
        self.read_frame_timeout(250)
    }

    fn read_frame_timeout(&mut self, timeout: i32) -> Result<Option<Value>> {
        if let Some(frame) = take_json(&mut self.acc) {
            return Ok(Some(frame));
        }
        let mut buf = vec![0u8; REPORT_SIZE];
        let n = self
            .dev
            .read_timeout(&mut buf, timeout)
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

    /// Raw frames are decoded by the adapter so layer notifications and key
    /// notifications preserve their wire order.
    pub fn poll_frame(&mut self) -> Result<Option<Value>> {
        self.poll_frame_timeout(0)
    }

    pub fn poll_frame_timeout(&mut self, timeout: i32) -> Result<Option<Value>> {
        if let Some(frame) = self.pending.pop_front() {
            return Ok(Some(frame));
        }
        self.read_frame_timeout(timeout)
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

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub profile: u16,
    pub position: u16,
    pub source: Option<String>,
    pub zone_lighting: ZoneLighting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ZoneLighting {
    Beckon,
    Firmware,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    pub refresh_ms: u64,
    pub effect_speed: f32,
    pub base: Treatment,
    pub ring: Treatment,
    pub ring_follows_state_color: bool,
    pub layers: BTreeMap<String, Layer>,
}

pub fn options(device: &DeviceConfig) -> Result<Options> {
    toml::Value::Table(device.definition.options.clone())
        .try_into()
        .with_context(|| format!("{}: options", device.path.display()))
}

pub const fn mutable_channels() -> &'static [&'static str] {
    &["base", "ring"]
}

pub fn apply_channels(
    device: &mut DeviceConfig,
    channels: &BTreeMap<String, Treatment>,
) -> Result<()> {
    let mut options = options(device)?;
    for (name, treatment) in channels {
        treatment.validate()?;
        match name.as_str() {
            "base" => options.base = *treatment,
            "ring" => options.ring = *treatment,
            _ => bail!(
                "unknown appearance channel {name:?} on {:?}; available: base, ring",
                device.id
            ),
        }
    }
    device.definition.options = toml::Value::try_from(options)?
        .as_table()
        .context("appearance options must be a table")?
        .clone();
    Ok(())
}

/// Addresses use a declared layer name plus the firmware id, e.g. rex/6.
/// Reserved ChatGPT ids never become writable, even through configuration.
fn parse_address<'a>(options: &'a Options, address: &str) -> Result<(&'a str, u8)> {
    let (layer, id) = address
        .split_once('/')
        .context("expected layer/id (e.g. rex/6)")?;
    let (name, _) = options
        .layers
        .get_key_value(layer)
        .with_context(|| format!("unknown layer {layer:?}"))?;
    let id: u8 = id.parse().context("agent id must be an integer")?;
    if !(6..=18).contains(&id) {
        bail!(
            "id {id} is not usable; AG00–AG05 are reserved, AG19+ unaddressable on tested firmware"
        );
    }
    Ok((name, id))
}

pub fn validate_device(device: &DeviceConfig, sources: &BTreeSet<&str>) -> Result<()> {
    let options = options(device)?;
    if options.refresh_ms == 0 {
        bail!("refresh_ms must be positive");
    }
    if !(0.0..=1.0).contains(&options.effect_speed) {
        bail!("effect_speed must be between 0 and 1 (firmware s field)");
    }
    options.base.validate()?;
    options.ring.validate()?;
    let mut selectors = BTreeSet::new();
    for (name, layer) in &options.layers {
        if !crate::device::valid_id(name)
            || layer.position == 0
            || !selectors.insert((layer.profile, layer.position))
        {
            bail!("layer {name}: invalid name, zero position or duplicate selector");
        }
        if let Some(source) = &layer.source {
            crate::device::require("source", source, sources.iter().copied())?;
        }
    }
    let mut addresses = BTreeSet::new();
    for member in device.definition.keys.values().flat_map(|key| &key.members) {
        let parsed = parse_address(&options, &member.address)
            .with_context(|| format!("{}: {:?}", device.path.display(), member.address))?;
        if !addresses.insert(parsed) {
            bail!("duplicate physical address {:?}", member.address);
        }
    }
    Ok(())
}

/// Separate online validation: config syntax/ranges can be checked while the
/// device is disconnected, but only its current keymap proves an id is mapped.
pub fn validate_keymap(device: &DeviceConfig, keymap: &Value) -> Result<()> {
    let options = options(device)?;
    for member in device.definition.keys.values().flat_map(|key| &key.members) {
        let (name, id) = parse_address(&options, &member.address)?;
        let selector = &options.layers[name];
        let profiles = keymap
            .get("profiles")
            .and_then(Value::as_array)
            .context("keymap has no profiles array")?;
        let profile = profiles
            .iter()
            .find(|profile| {
                profile.get("id").and_then(Value::as_u64) == Some(selector.profile as u64)
            })
            .with_context(|| format!("keymap has no profile id {}", selector.profile))?;
        let layer = profile
            .get("layers")
            .and_then(Value::as_array)
            .and_then(|layers| layers.get(selector.position as usize - 1))
            .with_context(|| format!("keymap has no layer position {}", selector.position))?;
        let rows = layer
            .pointer("/layout/keymap")
            .and_then(Value::as_array)
            .context("layer has no layout.keymap")?;
        let code = format!("KV_OAI_AG{id:02}");
        if !rows
            .iter()
            .filter_map(Value::as_array)
            .flatten()
            .any(|cell| cell.as_str() == Some(&code))
        {
            bail!(
                "{}:{} is configured but {code} is missing from profile {}, layer position {}; update the canonical keymap and verify its readback",
                device.id,
                member.address,
                selector.profile,
                selector.position
            );
        }
    }
    Ok(())
}

fn frame(id: u8, treatment: Option<Treatment>, speed: f32) -> Value {
    match treatment {
        Some(t) => {
            json!({"id":id,"c":rgb_u32(t.color),"b":t.brightness,"e":effect(t.motion),"s":speed})
        }
        None => json!({"id":id,"c":0,"b":0.0,"e":EFFECT_OFF,"s":speed}),
    }
}

#[derive(Clone, Debug, PartialEq)]
struct LightingFrame {
    zones: Option<Value>,
    entries: Value,
}

impl LightingFrame {
    fn commands(&self) -> impl Iterator<Item = (&'static str, Value)> + '_ {
        self.zones
            .iter()
            .cloned()
            .map(|zones| ("v.oai.rgbcfg", zones))
            .chain(std::iter::once(("v.oai.thstatus", self.entries.clone())))
    }
}

/// One handle owns both input and lighting. The adapter tracks layers, then
/// emits device-neutral addresses; the daemon never interprets a layer number.
pub struct CreatorMicroDisplay {
    device: DeviceConfig,
    options: Options,
    pad: Option<Pad>,
    active: Option<String>,
    selector: Option<(u16, u16)>,
    last_frame: Option<LightingFrame>,
    last_write: Option<Instant>,
    last_try: Option<Instant>,
    reset_pending: bool,
    binding_sources: BTreeMap<String, String>,
    clear_pending: Vec<u8>,
}

impl CreatorMicroDisplay {
    pub fn new(device: DeviceConfig) -> Result<Self> {
        let options = options(&device)?;
        Ok(Self {
            device,
            options,
            pad: None,
            active: None,
            selector: None,
            last_frame: None,
            last_write: None,
            last_try: None,
            reset_pending: false,
            binding_sources: BTreeMap::new(),
            clear_pending: Vec::new(),
        })
    }

    fn layer(&mut self, profile: u16, position: u16) {
        let selector = Some((profile, position));
        if self.selector != selector {
            if let Some(frame) = &self.last_frame {
                self.clear_pending.extend(
                    frame
                        .entries
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|entry| {
                            entry
                                .get("id")
                                .and_then(Value::as_u64)
                                .and_then(|id| u8::try_from(id).ok())
                        }),
                );
            }
            self.reset_pending = true;
            self.last_frame = None;
        }
        self.selector = selector;
        self.active = self
            .options
            .layers
            .iter()
            .find(|(_, layer)| layer.profile == profile && layer.position == position)
            .map(|(name, _)| name.clone());
    }

    fn connect(&mut self) -> Result<bool> {
        if self.pad.is_some() {
            return Ok(true);
        }
        if self
            .last_try
            .is_some_and(|last| last.elapsed() < Duration::from_secs(2))
        {
            return Ok(false);
        }
        self.last_try = Some(Instant::now());
        let Ok(mut pad) = Pad::open() else {
            return Ok(false);
        };
        let status = pad.call_json("device.status", json!(null))?;
        let profile = u16::try_from(
            status
                .get("profile_index")
                .and_then(Value::as_u64)
                .context("device.status omitted profile_index")?,
        )?;
        let layer = u16::try_from(
            status
                .get("layer_index")
                .and_then(Value::as_u64)
                .context("device.status omitted layer_index")?,
        )?;
        self.layer(profile, layer);
        // Notifications preceding this authoritative status response describe
        // old presses; replaying one on startup could trigger an unintended action.
        pad.pending.clear();
        self.pad = Some(pad);
        info!(device = %self.device.id, "connected Creator Micro");
        Ok(true)
    }

    fn lost(&mut self) {
        self.pad = None;
        self.active = None;
        self.selector = None;
        self.last_frame = None;
        self.reset_pending = true;
    }

    fn decode(&mut self, value: &Value) -> Option<DeviceEvent> {
        match value.get("m").and_then(Value::as_str)? {
            "kb.pl" => {
                let p = value.get("p")?;
                let profile = u16::try_from(p.get("p")?.as_u64()?).ok()?;
                let position = u16::try_from(p.get("l")?.as_u64()?).ok()?;
                if self.selector == Some((profile, position)) {
                    return None;
                }
                self.layer(profile, position);
                self.reset_pending = false;
                Some(DeviceEvent::Reset {
                    device: self.device.id.clone(),
                })
            }
            "v.oai.hid" => {
                let p = value.get("p")?;
                let key = p.get("k")?.as_str()?;
                let id = key.strip_prefix("AG")?.parse::<u8>().ok()?;
                let pressed = match p.get("act")?.as_u64()? {
                    1 => true,
                    0 => false,
                    _ => return None,
                };
                let active = self.active.as_ref()?;
                let address = Address {
                    device: self.device.id.clone(),
                    address: format!("{active}/{id}"),
                };
                // Match canonical addresses (6 and 06 mean the same firmware id).
                let (key, member) = self
                    .device
                    .definition
                    .keys
                    .values()
                    .flat_map(|key| key.members.iter().map(move |member| (key, member)))
                    .find(|(_, member)| {
                        parse_address(&self.options, &member.address).ok()
                            == Some((active.as_str(), id))
                    })?;
                if let (Some(binding), Some(source)) =
                    (&key.binding, &self.options.layers[active].source)
                    && self.binding_sources.get(binding) != Some(source)
                {
                    return None;
                }
                Some(DeviceEvent::Key {
                    address: if member.address == address.address {
                        address
                    } else {
                        member.clone()
                    },
                    pressed,
                })
            }
            _ => None,
        }
    }

    fn lighting(
        &self,
        plan: &RenderPlan,
        feedback: &BTreeMap<Address, Treatment>,
    ) -> Result<Option<LightingFrame>> {
        let Some(active) = &self.active else {
            return Ok(None);
        };
        let layer = &self.options.layers[active];
        let mut entries = Vec::new();
        let mut visible = Vec::new();
        for key in self.device.definition.keys.values() {
            let render = key
                .binding
                .as_ref()
                .and_then(|binding| plan.keys.iter().find(|render| &render.key == binding))
                .filter(|render| {
                    layer
                        .source
                        .as_ref()
                        .is_none_or(|source| render.source.as_ref() == Some(source))
                });
            for member in &key.members {
                let (name, id) = parse_address(&self.options, &member.address)?;
                if name != active {
                    continue;
                }
                if let Some(render) = render {
                    visible.push(render);
                }
                let treatment = feedback.get(member).copied().or_else(|| {
                    render.filter(|r| r.state.is_some()).map(|r| Treatment {
                        color: r.color,
                        brightness: r.brightness,
                        motion: r.motion,
                    })
                });
                entries.push(frame(id, treatment, self.options.effect_speed));
            }
        }
        let mut ring = self.options.ring;
        if self.options.ring_follows_state_color
            && let Some(state) = visible
                .into_iter()
                .filter_map(|r| r.state)
                .max_by_key(|s| s.attention_rank())
        {
            let treatment = plan.treatments.for_state(state);
            ring.color = treatment.color;
        }
        let zone = |t: Treatment| json!({"e":effect(t.motion),"b":t.brightness,"s":self.options.effect_speed,"m":1,"c":rgb_u32(t.color)});
        // AIDEV-NOTE: firmware-owned layers still get per-key feedback, never
        // Rex's device-level base/ring palette or an empty RGB-zone command.
        Ok(Some(LightingFrame {
            zones: (layer.zone_lighting == ZoneLighting::Beckon)
                .then(|| json!({"keys":zone(self.options.base),"ambient":zone(ring)})),
            entries: Value::Array(entries),
        }))
    }
}

impl DisplaySink for CreatorMicroDisplay {
    fn refresh_presentation(&mut self, device: &DeviceConfig) -> Result<()> {
        let options = options(device)?;
        self.options = options;
        self.device = device.clone();
        self.last_write = None;
        Ok(())
    }
    fn id(&self) -> &str {
        &self.device.id
    }
    fn poll_input(&mut self) -> Result<Vec<DeviceEvent>> {
        let mut events = Vec::new();
        if self.reset_pending {
            events.push(DeviceEvent::Reset {
                device: self.device.id.clone(),
            });
            self.reset_pending = false;
        }
        if !self.connect()? {
            return Ok(events);
        }
        if self.reset_pending {
            events.push(DeviceEvent::Reset {
                device: self.device.id.clone(),
            });
            self.reset_pending = false;
        }
        // Bounded draining preserves every press/release without starving IPC.
        for _ in 0..128 {
            match self.pad.as_mut().unwrap().poll_frame_timeout(0) {
                Ok(Some(value)) => {
                    if let Some(event) = self.decode(&value) {
                        events.push(event);
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    self.lost();
                    return Err(error);
                }
            }
        }
        Ok(events)
    }
    fn publish(&mut self, plan: &RenderPlan) -> Result<PublishOutcome> {
        self.publish_feedback(plan, &BTreeMap::new())
    }
    fn publish_feedback(
        &mut self,
        plan: &RenderPlan,
        feedback: &BTreeMap<Address, Treatment>,
    ) -> Result<PublishOutcome> {
        self.binding_sources = plan
            .keys
            .iter()
            .filter_map(|key| {
                key.source
                    .as_ref()
                    .map(|source| (key.key.clone(), source.clone()))
            })
            .collect();
        // Connect/poll happens before publish, so a layer reset reaches the
        // switch engine before any stale feedback can be written.
        if self.pad.is_none() {
            return Ok(PublishOutcome::Unavailable);
        }
        if !self.clear_pending.is_empty() {
            let entries = self
                .clear_pending
                .iter()
                .map(|id| frame(*id, None, self.options.effect_speed))
                .collect();
            if let Err(error) = self
                .pad
                .as_mut()
                .unwrap()
                .send_only("v.oai.thstatus", Value::Array(entries))
            {
                self.lost();
                return Err(error);
            }
            self.clear_pending.clear();
        }
        let Some(frame) = self.lighting(plan, feedback)? else {
            return Ok(PublishOutcome::Unchanged);
        };
        let refresh = Duration::from_millis(self.options.refresh_ms);
        if self.last_frame.as_ref() == Some(&frame)
            && self.last_write.is_some_and(|last| last.elapsed() < refresh)
        {
            return Ok(PublishOutcome::Unchanged);
        }
        let pad = self.pad.as_mut().unwrap();
        for (method, payload) in frame.commands() {
            if let Err(error) = pad.send_only(method, payload) {
                self.lost();
                return Err(error);
            }
        }
        self.last_frame = Some(frame);
        self.last_write = Some(Instant::now());
        debug!(device=%self.device.id, "published Creator Micro frame");
        Ok(PublishOutcome::Updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured() -> CreatorMicroDisplay {
        CreatorMicroDisplay::new(DeviceConfig {
            id: "creator-micro".into(),
            path: "creator-micro.toml".into(),
            definition: toml::from_str(include_str!("../templates/creator-micro.toml")).unwrap(),
        })
        .unwrap()
    }

    #[test]
    fn address_ranges_and_layer_selectors_are_validated_offline() {
        let display = configured();
        for id in ["rex/0", "rex/5", "rex/19", "rex/255", "missing/6", "6"] {
            assert!(parse_address(&display.options, id).is_err());
        }
        assert_eq!(
            parse_address(&display.options, "rex/16").unwrap(),
            ("rex", 16)
        );
    }

    #[test]
    fn layer_zone_lighting_ownership_is_typed_and_required() {
        let mut device = configured().device;
        let layers = device.definition.options["layers"].as_table_mut().unwrap();
        for (_, layer) in layers.iter_mut() {
            layer.as_table_mut().unwrap().insert(
                "zone_lighting".into(),
                toml::Value::String("firmware".into()),
            );
        }
        assert!(
            options(&device).is_ok(),
            "firmware-owned zone lighting must be configurable"
        );
        device.definition.options["layers"]["rex"]
            .as_table_mut()
            .unwrap()
            .remove("zone_lighting");
        assert!(
            options(&device).is_err(),
            "ownership must not have a hidden default"
        );
        device.definition.options["layers"]["rex"]
            .as_table_mut()
            .unwrap()
            .insert("zone_lighting".into(), toml::Value::String("both".into()));
        assert!(
            options(&device).is_err(),
            "two competing zone owners must be rejected"
        );
    }

    #[test]
    fn layer_notifications_translate_addresses_without_leaking_layers_into_core() {
        let mut display = configured();
        assert!(matches!(
            display.decode(&json!({"m":"kb.pl","p":{"p":1,"l":6}})),
            Some(DeviceEvent::Reset { .. })
        ));
        assert!(
            matches!(display.decode(&json!({"m":"v.oai.hid","p":{"act":1,"k":"AG16"}})), Some(DeviceEvent::Key{address,pressed:true}) if address.address == "test/16")
        );
        display.decode(&json!({"m":"kb.pl","p":{"p":1,"l":2}}));
        assert!(
            display
                .decode(&json!({"m":"v.oai.hid","p":{"act":1,"k":"AG16"}}))
                .is_none()
        );
    }

    #[test]
    fn source_filters_apply_to_navigation_and_lighting() {
        let mut display = configured();
        display.layer(1, 3);
        display
            .binding_sources
            .insert("key1".into(), "other".into());
        let press = json!({"m":"v.oai.hid","p":{"act":1,"k":"AG06"}});
        assert!(display.decode(&press).is_none());
        display.binding_sources.insert("key1".into(), "rex".into());
        assert!(matches!(
            display.decode(&press),
            Some(DeviceEvent::Key { .. })
        ));
        let mut plan = crate::render::demo_plan(&crate::render::DisplayConfig::default()).unwrap();
        plan.keys[0].key = "key1".into();
        plan.keys[0].source = Some("other".into());
        let entries = display
            .lighting(&plan, &BTreeMap::new())
            .unwrap()
            .unwrap()
            .entries;
        assert_eq!(entries[0]["e"], 0);
        plan.keys[0].source = Some("rex".into());
        let entries = display
            .lighting(&plan, &BTreeMap::new())
            .unwrap()
            .unwrap()
            .entries;
        assert_eq!(entries[0]["e"], 1);
    }

    #[test]
    fn feedback_paints_the_joined_pair_and_never_reserved_ids() {
        let mut display = configured();
        display.layer(1, 6);
        let t = Treatment {
            color: Rgb {
                red: 255,
                green: 0,
                blue: 0,
            },
            brightness: 1.0,
            motion: Motion::Breathe,
        };
        let feedback = [16, 17]
            .into_iter()
            .map(|id| {
                (
                    Address {
                        device: "creator-micro".into(),
                        address: format!("test/{id}"),
                    },
                    t,
                )
            })
            .collect();
        let plan = crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap();
        let entries = display.lighting(&plan, &feedback).unwrap().unwrap().entries;
        let entries = entries.as_array().unwrap();
        assert!(
            entries
                .iter()
                .all(|entry| entry["id"].as_u64().unwrap() >= 6)
        );
        assert_eq!(entries.iter().filter(|entry| entry["e"] == 4).count(), 2);
        display.layer(1, 2);
        assert!(display.lighting(&plan, &feedback).unwrap().is_none());
    }

    fn with_microphone_layers() -> DeviceConfig {
        let mut device = configured().device;
        for (name, position) in [("home", 1), ("herdr", 4), ("delta", 5)] {
            let layer = Layer {
                profile: 1,
                position,
                source: None,
                zone_lighting: ZoneLighting::Firmware,
            };
            device.definition.options["layers"]
                .as_table_mut()
                .unwrap()
                .insert(name.into(), toml::Value::try_from(layer).unwrap());
            for id in [16, 17] {
                device
                    .definition
                    .keys
                    .get_mut("key11")
                    .unwrap()
                    .members
                    .push(Address::try_from(format!("creator-micro:{name}/{id}")).unwrap());
            }
        }
        device
    }

    #[test]
    fn firmware_owned_layers_receive_only_joined_microphone_feedback() {
        let mut display = CreatorMicroDisplay::new(with_microphone_layers()).unwrap();
        let treatment = Treatment {
            color: "#FF0000".parse().unwrap(),
            brightness: 1.0,
            motion: Motion::Breathe,
        };
        let plan = crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap();
        for (name, position) in [("home", 1), ("herdr", 4), ("delta", 5)] {
            display.layer(1, position);
            let feedback = [16, 17]
                .into_iter()
                .map(|id| {
                    (
                        Address::try_from(format!("creator-micro:{name}/{id}")).unwrap(),
                        treatment,
                    )
                })
                .collect();
            let frame = display.lighting(&plan, &feedback).unwrap().unwrap();
            assert!(
                frame.zones.is_none(),
                "{name} must preserve its firmware zone lighting"
            );
            let commands = frame.commands().collect::<Vec<_>>();
            assert_eq!(commands.len(), 1);
            assert_eq!(commands[0].0, "v.oai.thstatus");
            assert_eq!(commands[0].1.as_array().unwrap().len(), 2);
            for (entry, id) in commands[0].1.as_array().unwrap().iter().zip([16, 17]) {
                assert_eq!(entry["id"], id);
                assert_eq!(entry["e"], EFFECT_BREATH);
            }
            let released = display.lighting(&plan, &BTreeMap::new()).unwrap().unwrap();
            assert!(released.zones.is_none());
            assert!(
                released
                    .entries
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|entry| entry["e"] == EFFECT_OFF)
            );
        }
    }

    #[test]
    fn beckon_owned_layers_keep_zone_and_key_commands() {
        let mut display = configured();
        display.layer(1, 3);
        let plan = crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap();
        let frame = display.lighting(&plan, &BTreeMap::new()).unwrap().unwrap();
        let methods = frame
            .commands()
            .map(|(method, _)| method)
            .collect::<Vec<_>>();
        assert_eq!(methods, ["v.oai.rgbcfg", "v.oai.thstatus"]);
        assert!(frame.zones.unwrap().get("keys").is_some());
    }

    #[test]
    fn four_layer_hold_control_releases_on_layer_changes_and_excludes_scratch() {
        use crate::switches::{Emitter, Switches};
        use std::{cell::RefCell, rc::Rc};
        struct Recording(Rc<RefCell<Vec<bool>>>);
        impl Emitter for Recording {
            fn emit(&mut self, _: &crate::chord::Chord, down: bool) -> Result<()> {
                self.0.borrow_mut().push(down);
                Ok(())
            }
        }
        fn route<E: Emitter>(event: DeviceEvent, switches: &mut Switches<E>) {
            match event {
                DeviceEvent::Reset { device } => switches.reset_device(&device).unwrap(),
                DeviceEvent::Key { address, pressed } => {
                    switches.event(&address, pressed, Instant::now()).unwrap()
                }
            }
        }

        let mut device = with_microphone_layers();
        device.definition.options["layers"]
            .as_table_mut()
            .unwrap()
            .remove("test");
        for key in device.definition.keys.values_mut() {
            key.members
                .retain(|member| !member.address.starts_with("test/"));
        }
        device
            .definition
            .keys
            .retain(|_, key| !key.members.is_empty());
        let crate::device::Action::Chord { mode, tap_ms, .. } =
            device.definition.actions.get_mut("dictate").unwrap();
        *mode = crate::device::Mode::Hold;
        *tap_ms = None;
        let calls = Rc::new(RefCell::new(Vec::new()));
        let mut switches = Switches::new(&[device.clone()], Recording(calls.clone())).unwrap();
        let mut display = CreatorMicroDisplay::new(device).unwrap();
        let plan = crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap();
        for position in [1, 3, 4, 5] {
            route(
                display
                    .decode(&json!({"m":"kb.pl","p":{"p":1,"l":position}}))
                    .unwrap(),
                &mut switches,
            );
            for id in [16, 17] {
                route(
                    display
                        .decode(&json!({"m":"v.oai.hid","p":{"act":1,"k":format!("AG{id}")}}))
                        .unwrap(),
                    &mut switches,
                );
            }
            assert!(switches.output_down());
            route(
                display
                    .decode(&json!({"m":"v.oai.hid","p":{"act":0,"k":"AG16"}}))
                    .unwrap(),
                &mut switches,
            );
            assert!(
                switches.output_down(),
                "rolling to the other half must keep the chord down"
            );
            route(
                display
                    .decode(&json!({"m":"kb.pl","p":{"p":1,"l":2}}))
                    .unwrap(),
                &mut switches,
            );
            assert!(!switches.output_down());
            assert!(
                display
                    .lighting(&plan, &switches.treatments())
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(
            *calls.borrow(),
            [true, false, true, false, true, false, true, false]
        );
        route(
            display
                .decode(&json!({"m":"kb.pl","p":{"p":1,"l":6}}))
                .unwrap(),
            &mut switches,
        );
        assert!(
            display
                .decode(&json!({"m":"v.oai.hid","p":{"act":1,"k":"AG16"}}))
                .is_none()
        );
        assert!(
            display
                .lighting(&plan, &switches.treatments())
                .unwrap()
                .is_none()
        );
    }

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
    fn take_json_survives_a_chunk_boundary() {
        let mut acc = String::from("{\"m\":\"v.oai.hid\",");
        let first = take_json(&mut acc);
        assert!(first.is_none());
        acc.push_str("\"p\":{\"k\":\"AG06\",\"act\":1}}");
        let value = take_json(&mut acc).unwrap();
        assert_eq!(value["p"]["k"], "AG06");
        assert!(acc.is_empty());
    }

    #[test]
    fn concatenated_frames_keep_every_edge_in_wire_order() {
        let mut acc = String::from(
            r#"{"m":"v.oai.hid","p":{"k":"AG16","act":1}}{"m":"v.oai.hid","p":{"k":"AG16","act":0}}"#,
        );
        assert_eq!(take_json(&mut acc).unwrap()["p"]["act"], 1);
        assert_eq!(take_json(&mut acc).unwrap()["p"]["act"], 0);
        assert!(acc.is_empty());
    }

    #[test]
    fn online_mapping_check_catches_missing_joined_members() {
        let display = configured();
        let codes: Vec<_> = (6..=17)
            .map(|id| json!(format!("KV_OAI_AG{id:02}")))
            .collect();
        let layer = json!({"layout":{"keymap":[codes]}});
        let mut keymap = json!({"profiles":[{"id":1,"layers":vec![layer;6]}]});
        validate_keymap(&display.device, &keymap).unwrap();
        keymap["profiles"][0]["layers"][5]["layout"]["keymap"][0][11] = json!("KC_ENT");
        assert!(
            validate_keymap(&display.device, &keymap)
                .unwrap_err()
                .to_string()
                .contains("AG17")
        );
    }

    #[test]
    fn appearance_updates_preserve_ids_needed_to_clear_a_departing_layer() {
        let mut display = configured();
        display.layer(1, 6);
        let plan = crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap();
        display.last_frame = display.lighting(&plan, &BTreeMap::new()).unwrap();
        let mut device = display.device.clone();
        let treatment = Treatment {
            color: "#123456".parse().unwrap(),
            brightness: 0.5,
            motion: Motion::Steady,
        };
        apply_channels(&mut device, &BTreeMap::from([("base".into(), treatment)])).unwrap();
        display.refresh_presentation(&device).unwrap();
        display.layer(1, 2);
        assert!(!display.clear_pending.is_empty());
        assert!(display.clear_pending.iter().all(|id| *id >= 6));
    }
}
