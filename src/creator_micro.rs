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
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
    task::Poll,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use hidapi::{HidApi, HidDevice};
use serde_json::{Value, json};
use tracing::debug;

use crate::{
    device::{Address, DeviceConfig, Treatment},
    display::{
        CaptureClaim, CaptureConsume, CaptureEvidence, CaptureId, CaptureReject, CaptureRequest,
        CaptureRequestOutcome, CaptureToken, Connectivity, DeviceEvent, DeviceObservation,
        DiagnosticCode, DisplaySink, InputBatch, InputContinuity, InputPollBudget, InputPollMode,
        LayerDescriptor, MemberDescriptor, ObservedLayer, PublishOutcome, ReconnectOutcome,
        Recovery, TransportEpoch, TransportStart, TypedFailure,
    },
    render::{Motion, RenderPlan, Rgb},
};

mod transport;
#[cfg(test)]
pub(crate) use transport::test_support;

const VENDOR_ID: u16 = 0x303A;
const REPORT_ID: u8 = 0x06;
const CHANNEL_RPC: u8 = 0x02;
const MAX_CHUNK: usize = 61;
const REPORT_SIZE: usize = 64;

type Clock = Rc<dyn Fn() -> Instant>;

#[derive(Debug, PartialEq, Eq)]
enum TransportContext {
    RequestSend,
    ExpectedAbsence,
    Retired,
}

impl std::fmt::Display for TransportContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::RequestSend => "RPC request send failed",
            Self::ExpectedAbsence => "no Creator Micro 2 on the HID bus (303A:8297 or 303A:8298)",
            Self::Retired => "Creator transport retired",
        })
    }
}

impl std::error::Error for TransportContext {}

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

// Small system seam: tests supply raw HID reports, never open a real device.
trait ReportIo {
    fn write(&self, report: &[u8]) -> Result<usize>;
    fn read_timeout(&self, report: &mut [u8], timeout: i32) -> Result<usize>;
    fn actor_write(&self, report: &[u8]) -> Poll<Result<usize>> {
        Poll::Ready(self.write(report))
    }
    fn actor_read(&self, report: &mut [u8]) -> Poll<Result<usize>> {
        Poll::Ready(self.read_timeout(report, 0))
    }
    fn actor_close(&self) -> Poll<()> {
        Poll::Ready(())
    }
}

impl ReportIo for HidDevice {
    fn write(&self, report: &[u8]) -> Result<usize> {
        Ok(HidDevice::write(self, report)?)
    }
    fn read_timeout(&self, report: &mut [u8], timeout: i32) -> Result<usize> {
        Ok(HidDevice::read_timeout(self, report, timeout)?)
    }
}

pub struct Pad {
    dev: Box<dyn ReportIo>,
    next_id: i64,
    actor_ids: Option<Arc<AtomicI64>>,
    acc: String,
    acc_times: VecDeque<(usize, Instant)>,
    pending: VecDeque<TimedFrame>,
    clock: Clock,
    last_input_success: Option<Instant>,
    last_output_success: Option<Instant>,
    probe_stop: Option<Arc<AtomicBool>>,
}

struct TimedFrame {
    value: Value,
    observed_at: Instant,
}

struct ReadStep {
    frame: Result<Option<TimedFrame>>,
    report_read: bool,
    empty: bool,
}

trait Discovery {
    type Candidate;
    fn enumerate(&mut self, stop: Option<&AtomicBool>) -> Result<Vec<Self::Candidate>>;
    fn open(&mut self, candidate: &Self::Candidate) -> Result<Box<dyn ReportIo>>;
    fn clock(&self) -> Clock;
}

#[derive(Default)]
struct NativeDiscovery {
    api: Option<HidApi>,
}
impl Discovery for NativeDiscovery {
    type Candidate = std::ffi::CString;
    fn enumerate(&mut self, stop: Option<&AtomicBool>) -> Result<Vec<Self::Candidate>> {
        discovery_active(stop)?;
        let result = HidApi::new().context("hidapi init");
        discovery_active(stop)?;
        let api = result?;
        // AIDEV-NOTE: hidapi defaults to an exclusive (seizing) open on macOS.
        // Non-exclusive is required so Input, ChatGPT and Beckon coexist, and
        // requires macOS Input Monitoring for the calling application.
        api.set_open_exclusive(false);
        discovery_active(stop)?;
        let candidates = api
            .device_list()
            .filter(|info| {
                info.vendor_id() == VENDOR_ID && matches!(info.product_id(), 0x8297 | 0x8298)
            })
            .map(|info| info.path().to_owned())
            .collect();
        self.api = Some(api);
        Ok(candidates)
    }
    fn open(&mut self, candidate: &Self::Candidate) -> Result<Box<dyn ReportIo>> {
        Ok(Box::new(
            self.api
                .as_ref()
                .context("discovery not initialized")?
                .open_path(candidate)?,
        ))
    }
    fn clock(&self) -> Clock {
        Rc::new(Instant::now)
    }
}

fn discovery_active(stop: Option<&AtomicBool>) -> Result<()> {
    if stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
        bail!(TransportContext::Retired);
    }
    Ok(())
}

impl Pad {
    /// The vendor collection is not the first USB interface, so try every
    /// interface the vendor exposes and keep the one that answers RPC.
    pub fn open() -> Result<Self> {
        Self::open_with_ids(None)
    }

    fn open_with_ids(actor_ids: Option<Arc<AtomicI64>>) -> Result<Self> {
        Self::discover_with(&mut NativeDiscovery::default(), actor_ids, None)
    }

    fn discover_with<D: Discovery>(
        native: &mut D,
        actor_ids: Option<Arc<AtomicI64>>,
        probe_stop: Option<Arc<AtomicBool>>,
    ) -> Result<Self> {
        discovery_active(probe_stop.as_deref())?;
        let result = native.enumerate(probe_stop.as_deref());
        discovery_active(probe_stop.as_deref())?;
        let candidates = result?;
        if candidates.is_empty() {
            bail!(TransportContext::ExpectedAbsence);
        }

        let mut last = String::new();
        for candidate in candidates {
            discovery_active(probe_stop.as_deref())?;
            let result = native.open(&candidate);
            // AIDEV-NOTE: stop cannot cancel native open; once it returns,
            // retire its handle on THIS owner before any probe or next open.
            discovery_active(probe_stop.as_deref())?;
            let dev = match result {
                Ok(dev) => dev,
                Err(err) => {
                    last = format!("open failed: {err}");
                    continue;
                }
            };
            let mut pad = Pad {
                dev,
                next_id: 1,
                actor_ids: actor_ids.clone(),
                acc: String::new(),
                acc_times: VecDeque::new(),
                pending: VecDeque::new(),
                clock: native.clock(),
                last_input_success: None,
                last_output_success: None,
                probe_stop: probe_stop.clone(),
            };
            let probe = pad.call("sys.version", json!(null));
            pad.ensure_active()?;
            match probe {
                Ok(_) => return Ok(pad),
                Err(err) => last = format!("{err:#}"),
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

    fn ensure_active(&self) -> Result<()> {
        discovery_active(self.probe_stop.as_deref())
    }

    /// Public RPC access for the filesystem transport in
    /// [`crate::creator_micro_fs`]. Kept on `Pad` so one HID handle is shared.
    pub fn call_json(&mut self, method: &str, params: Value) -> Result<Value> {
        Ok(self.call_timed(method, params)?.value)
    }

    fn call_timed(&mut self, method: &str, params: Value) -> Result<TimedFrame> {
        let id = self
            .send_request(method, params)
            .context(TransportContext::RequestSend)?;
        // AIDEV-NOTE: open() probes several vendor interfaces and most never
        // answer RPC, so the probe uses a short deadline. Filesystem writes
        // move 3KB chunks and need more room; use a separate per-method budget.
        let budget = if method.starts_with("fs.") {
            Duration::from_secs(5)
        } else {
            Duration::from_millis(700)
        };
        let deadline = (self.clock)() + budget;
        loop {
            self.ensure_active()?;
            if (self.clock)() >= deadline {
                bail!("timed out waiting for {method}");
            }
            let result = self.take_frame();
            self.ensure_active()?;
            let Some(frame) = result? else {
                continue;
            };
            let value = &frame.value;
            if value.get("id").and_then(Value::as_i64) != Some(id) {
                if value.get("m").is_some() {
                    if self.actor_ids.is_some() && self.pending.len() >= transport::EVENT_CAP {
                        bail!("Creator probe notification queue exceeded bound");
                    }
                    self.pending.push_back(frame);
                }
                continue;
            }
            if let Some(err) = value.get("error") {
                bail!("device returned error: {err}");
            }
            return Ok(TimedFrame {
                value: value.get("result").cloned().unwrap_or(Value::Null),
                observed_at: frame.observed_at,
            });
        }
    }

    /// Fire-and-forget lighting write. Never waits for a reply.
    pub fn send_only(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(method, params)
    }

    fn send(&mut self, method: &str, params: Value) -> Result<()> {
        self.send_request(method, params).map(|_| ())
    }

    fn send_request(&mut self, method: &str, params: Value) -> Result<i64> {
        self.ensure_active()?;
        let id = if let Some(ids) = &self.actor_ids {
            ids.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_add(1).filter(|next| *next > 0)
            })
            .map_err(|_| anyhow::anyhow!("Creator request id exhausted"))?
        } else {
            let id = self.next_id;
            self.next_id = (self.next_id % 998) + 1;
            id
        };
        let message = json!({ "method": method, "id": id, "params": params });
        let bytes = serde_json::to_vec(&message)?;
        let mut offset = 0;
        while offset < bytes.len() {
            self.ensure_active()?;
            let n = MAX_CHUNK.min(bytes.len() - offset);
            let mut report = vec![0u8; REPORT_SIZE];
            report[0] = REPORT_ID;
            report[1] = CHANNEL_RPC;
            report[2] = n as u8;
            report[3..3 + n].copy_from_slice(&bytes[offset..offset + n]);
            let result = self
                .dev
                .write(&report)
                .context("IOHIDDeviceSetReport failed");
            self.ensure_active()?;
            let written = result?;
            if written != report.len() {
                bail!("incomplete HID output report");
            }
            offset += n;
        }
        self.last_output_success = Some((self.clock)());
        Ok(id)
    }

    /// Read one HID report and return the next complete JSON frame, if any.
    fn take_frame(&mut self) -> Result<Option<TimedFrame>> {
        self.read_frame_timeout(250)
    }

    fn read_frame_timeout(&mut self, timeout: i32) -> Result<Option<TimedFrame>> {
        self.read_step_timeout(timeout)?.frame
    }

    fn buffered_frame(&mut self) -> Result<Option<TimedFrame>> {
        let before = self.acc.len();
        let parsed = take_json(&mut self.acc);
        let mut consumed = before - self.acc.len();
        let mut observed_at = None;
        // AIDEV-NOTE: byte spans preserve the completing read's timestamp even
        // across fragments and multiple frames decoded on later daemon turns.
        while consumed > 0 {
            let (bytes, at) = self.acc_times.front_mut().expect("timestamped RPC bytes");
            observed_at = Some(*at);
            let used = consumed.min(*bytes);
            consumed -= used;
            *bytes -= used;
            if *bytes == 0 {
                self.acc_times.pop_front();
            }
        }
        parsed.map(|frame| {
            frame.map(|value| TimedFrame {
                value,
                observed_at: observed_at.expect("complete frame has read evidence"),
            })
        })
    }

    // AIDEV-NOTE: None after partial JSON is progress, not would-block. Keep
    // the raw-report count distinct from the number of decoded notifications.
    fn read_step_timeout(&mut self, timeout: i32) -> Result<ReadStep> {
        self.ensure_active()?;
        let buffered = self.buffered_frame();
        if !matches!(buffered, Ok(None)) {
            return Ok(ReadStep {
                frame: buffered,
                report_read: false,
                empty: false,
            });
        }
        let mut buf = vec![0u8; REPORT_SIZE];
        let result = self
            .dev
            .read_timeout(&mut buf, timeout)
            .context("IOHIDDeviceGetReport failed");
        self.ensure_active()?;
        let n = result?;
        let step = self.ingest_report(&buf[..n]);
        if self.actor_ids.is_some() && self.acc.len() > transport::ACC_CAP {
            bail!("Creator RPC accumulator exceeded bound");
        }
        Ok(step)
    }

    fn ingest_report(&mut self, report: &[u8]) -> ReadStep {
        if report.is_empty() {
            return ReadStep {
                frame: Ok(None),
                report_read: false,
                empty: true,
            };
        }
        self.last_input_success = Some((self.clock)());
        let Some(text) = report_text(report) else {
            return ReadStep {
                frame: Ok(None),
                report_read: true,
                empty: false,
            };
        };
        self.acc.push_str(text);
        self.acc_times
            .push_back((text.len(), self.last_input_success.unwrap()));
        ReadStep {
            frame: self.buffered_frame(),
            report_read: true,
            empty: false,
        }
    }

    /// Raw frames are decoded by the adapter so layer notifications and key
    /// notifications preserve their wire order.
    pub fn poll_frame(&mut self) -> Result<Option<Value>> {
        self.poll_frame_timeout(0)
    }

    pub fn poll_frame_timeout(&mut self, timeout: i32) -> Result<Option<Value>> {
        Ok(self.poll_timed_frame(timeout)?.map(|frame| frame.value))
    }

    fn poll_timed_frame(&mut self, timeout: i32) -> Result<Option<TimedFrame>> {
        if let Some(frame) = self.pending.pop_front() {
            return Ok(Some(frame));
        }
        self.read_frame_timeout(timeout)
    }

    fn poll_step(&mut self) -> Result<ReadStep> {
        if let Some(frame) = self.pending.pop_front() {
            return Ok(ReadStep {
                frame: Ok(Some(frame)),
                report_read: false,
                empty: false,
            });
        }
        self.read_step_timeout(0)
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
fn take_json(acc: &mut String) -> Result<Option<Value>> {
    let Some(start) = acc.find('{') else {
        return Ok(None);
    };
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
                    return serde_json::from_str(&text)
                        .map(Some)
                        .context("malformed RPC JSON frame");
                }
            }
            _ => {}
        }
    }
    Ok(None)
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
    transport: Option<transport::Endpoint>,
    transport_starter: Box<dyn transport::Starter>,
    transport_applied_epoch: Option<crate::display::TransportEpoch>,
    transport_orphaned_clears: u16,
    device: DeviceConfig,
    options: Options,
    pad: Option<Pad>,
    // At most one lost handle; only legacy reconnect/adapter teardown may close
    // it. hidapi's macOS Drop joins its reader and is not a bounded drain step.
    retired_pad: Option<Pad>,
    active: Option<String>,
    selector: Option<(u16, u16)>,
    last_frame: Option<LightingFrame>,
    last_write: Option<Instant>,
    #[cfg(test)]
    last_try: Option<Instant>,
    reset_pending: bool,
    binding_sources: BTreeMap<String, String>,
    clear_pending: Vec<u8>,
    clear_pending_origin: Option<TransportEpoch>,
    observation: DeviceObservation,
    clock: Clock,
}

impl Drop for CreatorMicroDisplay {
    fn drop(&mut self) {
        if let Some(endpoint) = &self.transport {
            endpoint.stop();
        }
    }
}

impl CreatorMicroDisplay {
    pub fn new(device: DeviceConfig) -> Result<Self> {
        let options = options(&device)?;
        Ok(Self {
            transport: None,
            transport_starter: Box::new(transport::ProductionStarter),
            transport_applied_epoch: None,
            transport_orphaned_clears: 0,
            device,
            options,
            pad: None,
            retired_pad: None,
            active: None,
            selector: None,
            last_frame: None,
            last_write: None,
            #[cfg(test)]
            last_try: None,
            reset_pending: false,
            binding_sources: BTreeMap::new(),
            clear_pending: Vec::new(),
            clear_pending_origin: None,
            observation: DeviceObservation {
                observation_supported: true,
                ..Default::default()
            },
            clock: Rc::new(Instant::now),
        })
    }

    #[cfg(test)]
    fn layer(&mut self, profile: u16, position: u16) {
        self.layer_at(profile, position, (self.clock)());
    }

    fn layer_at(&mut self, profile: u16, position: u16, observed_at: Instant) {
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
                self.clear_pending_origin = self.transport_applied_epoch;
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
        self.observation.connectivity = Connectivity::Connected;
        self.observation.observed_at = self.observation.observed_at.max(Some(observed_at));
        self.observation.layer = Some(ObservedLayer {
            name: self.active.clone(),
            profile: Some(profile),
            position: Some(position),
            observed_at,
        });
    }

    fn observed_input(&mut self) {
        let Some(now) = self.pad.as_ref().and_then(|pad| pad.last_input_success) else {
            return;
        };
        self.observation.connectivity = Connectivity::Connected;
        self.observation.observed_at = self.observation.observed_at.max(Some(now));
        self.observation.last_input_success = Some(now);
        self.recovered(DiagnosticCode::InputFailed);
    }

    fn same_transport_connection(a: Option<TransportEpoch>, b: Option<TransportEpoch>) -> bool {
        match (a, b) {
            (Some(a), Some(b)) => a.actor == b.actor && a.connection == b.connection,
            (None, None) => true,
            _ => false,
        }
    }

    fn orphan_frontend_clears(&mut self) {
        for id in self.clear_pending.drain(..) {
            if (6..=18).contains(&id) {
                self.transport_orphaned_clears |= 1 << (id - 6);
            }
        }
        self.clear_pending_origin = None;
    }

    fn recovered(&mut self, code: DiagnosticCode) {
        if let Some(failure) = &mut self.observation.last_failure
            && failure.code == code
        {
            failure.active = false;
        }
    }

    fn observed_output(&mut self) {
        let Some(now) = self.pad.as_ref().and_then(|pad| pad.last_output_success) else {
            return;
        };
        self.observation.connectivity = Connectivity::Connected;
        self.observation.observed_at = self.observation.observed_at.max(Some(now));
        self.observation.last_output_success = Some(now);
        self.recovered(DiagnosticCode::OutputFailed);
    }

    fn failed(&mut self, code: DiagnosticCode) {
        self.lost();
        self.observation.last_failure = Some(TypedFailure {
            code,
            at: (self.clock)(),
            active: true,
            recovery: Recovery::ReconnectDevice,
        });
    }

    fn input_loss_boundary(&mut self, batch: &mut InputBatch) {
        batch.partial_frame = !self.pad.as_ref().unwrap().acc.trim().is_empty();
        debug_assert!(self.retired_pad.is_none());
        self.retired_pad = self.pad.take();
        self.failed(DiagnosticCode::InputFailed);
        // Preserve already-read releases/edges before the typed loss boundary.
        batch.events.push(DeviceEvent::Reset {
            device: self.device.id.clone(),
        });
        self.reset_pending = false;
        batch.continuity = InputContinuity::Lost;
    }

    // Legacy discovery remains explicit and blocking. Tests inject only the
    // system seam; the new scheduling interface never invokes this function.
    #[cfg(test)]
    fn connect_with(&mut self, open: impl FnOnce() -> Result<Pad>) -> Result<bool> {
        if self.transport.is_some() {
            return Ok(self.diagnostics().connectivity == Connectivity::Connected);
        }
        if self.pad.is_some() {
            return Ok(true);
        }
        self.retired_pad.take(); // Legacy path only, never called by DrainOnly.
        if self.last_try.is_some_and(|last| {
            (self.clock)().saturating_duration_since(last) < Duration::from_secs(2)
        }) {
            return Ok(false);
        }
        self.last_try = Some((self.clock)());
        let mut pad = match open() {
            Ok(pad) => pad,
            Err(error) => {
                let absent = error.downcast_ref::<TransportContext>()
                    == Some(&TransportContext::ExpectedAbsence);
                self.observation.connectivity = if absent {
                    Connectivity::Disconnected
                } else {
                    Connectivity::Unknown
                };
                self.observation.observed_at = Some((self.clock)());
                if absent {
                    self.observation.last_failure = Some(TypedFailure {
                        code: DiagnosticCode::DeviceUnavailable,
                        at: (self.clock)(),
                        active: false,
                        recovery: Recovery::None,
                    });
                }
                // Opaque errors establish no typed cause; retain prior history.
                return Ok(false);
            }
        };
        pad.clock = self.clock.clone();
        let status = pad.call_timed("device.status", json!(null));
        self.pad = Some(pad);
        self.observed_output();
        self.observed_input();
        let selector = status.and_then(|frame| -> Result<(u16, u16, Instant)> {
            let status = frame.value;
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
            if layer == 0 {
                bail!("device.status layer_index must be one-based");
            }
            Ok((profile, layer, frame.observed_at))
        });
        let (profile, layer, observed_at) = match selector {
            Ok(selector) => selector,
            Err(error) => {
                self.failed(
                    if error.downcast_ref::<TransportContext>()
                        == Some(&TransportContext::RequestSend)
                    {
                        DiagnosticCode::OutputFailed
                    } else {
                        DiagnosticCode::InputFailed
                    },
                );
                return Err(error);
            }
        };
        self.layer_at(profile, layer, observed_at);
        self.recovered(DiagnosticCode::DeviceUnavailable);
        // Notifications preceding this authoritative status response describe
        // old presses; replaying one on startup could trigger an unintended action.
        self.pad.as_mut().unwrap().pending.clear();
        tracing::info!(device = %self.device.id, "connected Creator Micro");
        Ok(true)
    }

    fn lost(&mut self) {
        self.pad = None;
        self.active = None;
        self.selector = None;
        self.last_frame = None;
        self.reset_pending = true;
        self.observation.connectivity = Connectivity::Disconnected;
        self.observation.observed_at = Some((self.clock)());
        self.observation.layer = None;
    }

    #[cfg(test)]
    fn decode(&mut self, value: &Value) -> Option<DeviceEvent> {
        self.decode_at(value, (self.clock)())
    }

    fn decode_at(&mut self, value: &Value, observed_at: Instant) -> Option<DeviceEvent> {
        match value.get("m").and_then(Value::as_str)? {
            "kb.pl" => {
                let p = value.get("p")?;
                let profile = u16::try_from(p.get("p")?.as_u64()?).ok()?;
                let position = u16::try_from(p.get("l")?.as_u64()?).ok()?;
                if position == 0 {
                    return None;
                }
                if self.selector == Some((profile, position)) {
                    self.layer_at(profile, position, observed_at);
                    return None;
                }
                self.layer_at(profile, position, observed_at);
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
    fn transport_epoch(&self) -> Option<TransportEpoch> {
        self.transport.as_ref().and_then(|e| e.epoch())
    }
    fn request_capture(&mut self, request: CaptureRequest) -> CaptureRequestOutcome {
        self.transport.as_ref().map_or(
            CaptureRequestOutcome::Rejected(CaptureReject::NotStarted),
            |e| e.request(request, (self.clock)()),
        )
    }
    fn capture_evidence(&self, id: CaptureId) -> Option<CaptureEvidence> {
        self.transport
            .as_ref()
            .and_then(|e| e.evidence(id, (self.clock)()))
    }
    fn release_capture(&mut self, token: CaptureToken) {
        if let Some(e) = &self.transport {
            e.release(token);
        }
    }
    fn try_claim_capture(&mut self, evidence: CaptureEvidence, now: Instant) -> CaptureClaim {
        self.transport
            .as_ref()
            .map_or(CaptureClaim::Rejected(CaptureReject::NotStarted), |e| {
                e.claim(evidence, now)
            })
    }
    fn consume_capture(
        &mut self,
        token: CaptureToken,
        applied_through: u64,
        now: Instant,
    ) -> CaptureConsume {
        self.transport
            .as_ref()
            .map_or(CaptureConsume::Rejected(CaptureReject::NotStarted), |e| {
                e.consume(token, applied_through, now)
            })
    }
    fn start_transport(&mut self) -> Result<TransportStart> {
        if let Some(endpoint) = &self.transport {
            return Ok(if endpoint.stopping() {
                TransportStart::Retiring
            } else {
                TransportStart::Running
            });
        }
        if self.pad.is_some() || self.retired_pad.is_some() {
            bail!("legacy Creator handle already owned");
        }
        let (outcome, endpoint) = self.transport_starter.start()?;
        self.transport = endpoint;
        Ok(outcome)
    }
    fn stop_transport(&mut self) {
        if let Some(endpoint) = &self.transport {
            endpoint.stop();
        }
    }
    fn reconnect_step(&mut self, deadline: Instant) -> Result<ReconnectOutcome> {
        if let Some(endpoint) = &self.transport {
            return Ok(if (self.clock)() >= deadline {
                ReconnectOutcome::Deferred
            } else if endpoint
                .observation()
                .is_some_and(|o| o.connectivity == Connectivity::Connected)
            {
                ReconnectOutcome::Connected
            } else {
                ReconnectOutcome::InProgress
            });
        }
        // AIDEV-NOTE: macOS HIDAPI open waits on a reader-thread barrier;
        // write and close are synchronous too. No <=5ms acquisition/handshake
        // primitive is available here. Never hide legacy connect() in this seam.
        // This reuses prior status evidence only; fresh startup/loss is Deferred
        // until a separately reviewed bounded platform lifecycle exists.
        Ok(
            if (self.clock)() < deadline
                && self.pad.is_some()
                && self.observation.connectivity == Connectivity::Connected
                && self.observation.layer.is_some()
            {
                ReconnectOutcome::Connected
            } else {
                ReconnectOutcome::Deferred
            },
        )
    }
    fn diagnostics(&self) -> DeviceObservation {
        let mut observation = self
            .transport
            .as_ref()
            .and_then(|e| e.observation())
            .unwrap_or_else(|| self.observation.clone());
        if let Some(layer) = &mut observation.layer {
            layer.name = self
                .options
                .layers
                .iter()
                .find(|(_, configured)| {
                    Some(configured.profile) == layer.profile
                        && Some(configured.position) == layer.position
                })
                .map(|(name, _)| name.clone());
        }
        observation.member_descriptors = self
            .device
            .definition
            .keys
            .values()
            .flat_map(|key| &key.members)
            .map(|address| MemberDescriptor {
                address: address.clone(),
                name: None,
                layer: parse_address(&self.options, &address.address)
                    .ok()
                    .map(|(name, _)| {
                        let layer = &self.options.layers[name];
                        LayerDescriptor {
                            name: name.into(),
                            profile: Some(layer.profile),
                            position: Some(layer.position),
                        }
                    }),
            })
            .collect();
        observation
    }
    fn poll_input_batch(
        &mut self,
        _mode: InputPollMode,
        budget: InputPollBudget,
    ) -> Result<InputBatch> {
        if self.transport.is_some()
            && !self.clear_pending.is_empty()
            && self.clear_pending_origin.is_none()
        {
            self.clear_pending_origin = self.transport_applied_epoch;
        }
        if let Some(endpoint) = &self.transport {
            if budget.max_raw_frames == 0 || (self.clock)() >= budget.deadline {
                return Ok(InputBatch::default());
            }
            let transport::Delivery {
                records,
                frontier,
                fault,
                partial,
                continuity,
            } = endpoint.poll(budget.max_raw_frames.min(128));
            let mut batch = InputBatch {
                partial_frame: partial,
                continuity,
                ..Default::default()
            };
            if fault {
                batch.continuity = InputContinuity::Lost;
            }
            for record in records {
                // AIDEV-NOTE: retire the previous session's ownership BEFORE
                // assigning a new epoch or deriving its selector/frame state.
                if !Self::same_transport_connection(
                    self.transport_applied_epoch,
                    Some(record.epoch),
                ) {
                    if !Self::same_transport_connection(
                        self.clear_pending_origin,
                        Some(record.epoch),
                    ) {
                        self.orphan_frontend_clears();
                    }
                    self.last_frame = None;
                    self.active = None;
                    self.selector = None;
                }
                self.transport_applied_epoch = Some(record.epoch);
                match record.frame {
                    Some(frame) => {
                        if let Some(event) = self.decode_at(&frame.value, frame.observed_at)
                            && (!fault || !matches!(event, DeviceEvent::Key { pressed: true, .. }))
                        {
                            batch.events.push(event);
                        }
                    }
                    None => {
                        if record.connection_lost {
                            self.active = None;
                            self.selector = None;
                            self.orphan_frontend_clears();
                        }
                        self.last_frame = None;
                        batch.continuity = InputContinuity::Lost;
                        batch.events.push(DeviceEvent::Reset {
                            device: self.device.id.clone(),
                        });
                    }
                }
            }
            batch.transport_epoch = self.transport_applied_epoch;
            batch.event_frontier = frontier;
            if batch.continuity != InputContinuity::Lost
                && batch
                    .events
                    .iter()
                    .any(|e| matches!(e, DeviceEvent::Reset { .. }))
            {
                batch.continuity = InputContinuity::Reset;
            }
            return Ok(batch);
        }
        let mut batch = InputBatch::default();
        if self.reset_pending {
            batch.events.push(DeviceEvent::Reset {
                device: self.device.id.clone(),
            });
            self.reset_pending = false;
        }
        if self.pad.is_none() {
            return Ok(batch);
        }
        batch.continuity = InputContinuity::Continuous;
        // Bound buffered-message work too: one report may contain multiple JSON
        // frames, and old RPC notifications may already be queued in memory.
        for _ in 0..budget.max_raw_frames.min(128) {
            if (self.clock)() >= budget.deadline {
                break;
            }
            let step = match self.pad.as_mut().unwrap().poll_step() {
                Ok(step) => step,
                Err(_) => {
                    self.input_loss_boundary(&mut batch);
                    return Ok(batch);
                }
            };
            if step.report_read {
                batch.raw_frames += 1;
            }
            self.observed_input();
            let value = match step.frame {
                Ok(value) => value,
                Err(_) => {
                    self.input_loss_boundary(&mut batch);
                    return Ok(batch);
                }
            };
            if let Some(value) = value
                && let Some(event) = self.decode_at(&value.value, value.observed_at)
            {
                batch.events.push(event);
            }
            if step.empty {
                batch.drained = self.pad.as_ref().unwrap().acc.trim().is_empty()
                    && (self.clock)() < budget.deadline;
                break;
            }
        }
        batch.partial_frame = !self.pad.as_ref().unwrap().acc.trim().is_empty();
        if batch
            .events
            .iter()
            .any(|event| matches!(event, DeviceEvent::Reset { .. }))
        {
            batch.continuity = InputContinuity::Reset;
        }
        Ok(batch)
    }
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
        if self.transport.is_some() {
            return Ok(self
                .poll_input_batch(
                    InputPollMode::DrainOnly,
                    InputPollBudget {
                        deadline: (self.clock)() + Duration::from_millis(5),
                        max_raw_frames: 128,
                    },
                )?
                .events);
        }
        let mut events = Vec::new();
        if self.reset_pending {
            events.push(DeviceEvent::Reset {
                device: self.device.id.clone(),
            });
            self.reset_pending = false;
        }
        // Explicit lifecycle is mandatory. Never discover/open as a polling
        // fallback; public Pad::open remains the separate direct CLI path.
        if self.pad.is_none() {
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
            let result = self.pad.as_mut().unwrap().poll_timed_frame(0);
            self.observed_input();
            match result {
                Ok(Some(value)) => {
                    if let Some(event) = self.decode_at(&value.value, value.observed_at) {
                        events.push(event);
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    self.failed(DiagnosticCode::InputFailed);
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
        if self.transport.is_some() && !self.clear_pending.is_empty() {
            if self.clear_pending_origin.is_none() {
                self.clear_pending_origin = self.transport_applied_epoch;
            }
            if !Self::same_transport_connection(
                self.clear_pending_origin,
                self.transport_applied_epoch,
            ) {
                self.orphan_frontend_clears();
            }
        }
        self.binding_sources = plan
            .keys
            .iter()
            .filter_map(|key| {
                key.source
                    .as_ref()
                    .map(|source| (key.key.clone(), source.clone()))
            })
            .collect();
        if let Some(endpoint) = &self.transport {
            let frame = self.lighting(plan, feedback)?;
            let (outcome, accepted) = endpoint.submit(
                self.transport_applied_epoch,
                frame.clone(),
                &self.clear_pending,
                self.options.effect_speed,
                (self.clock)(),
                Duration::from_millis(self.options.refresh_ms),
            )?;
            if accepted {
                self.clear_pending.clear();
                self.clear_pending_origin = None;
                self.last_frame = frame;
            }
            return Ok(outcome);
        }
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
                self.failed(DiagnosticCode::OutputFailed);
                return Err(error);
            }
            self.observed_output();
            self.clear_pending.clear();
        }
        let Some(frame) = self.lighting(plan, feedback)? else {
            return Ok(PublishOutcome::Unchanged);
        };
        let refresh = Duration::from_millis(self.options.refresh_ms);
        if self.last_frame.as_ref() == Some(&frame)
            && self
                .last_write
                .is_some_and(|last| (self.clock)().saturating_duration_since(last) < refresh)
        {
            return Ok(PublishOutcome::Unchanged);
        }
        for (method, payload) in frame.commands() {
            if let Err(error) = self.pad.as_mut().unwrap().send_only(method, payload) {
                self.failed(DiagnosticCode::OutputFailed);
                return Err(error);
            }
            self.observed_output();
        }
        self.last_frame = Some(frame);
        self.last_write = Some((self.clock)());
        debug!(device=%self.device.id, "published Creator Micro frame");
        Ok(PublishOutcome::Updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::{InputContinuity, InputPollBudget, InputPollMode};
    use std::cell::{Cell, RefCell};

    struct FakeIo {
        reads: RefCell<VecDeque<Result<Vec<u8>>>>,
        writes: RefCell<Vec<Vec<u8>>>,
        timeouts: RefCell<Vec<i32>>,
        time: Rc<Cell<Instant>>,
        step: Cell<Duration>,
        fail_write: Cell<bool>,
        short_write: Cell<bool>,
    }

    impl ReportIo for Rc<FakeIo> {
        fn read_timeout(&self, buffer: &mut [u8], timeout: i32) -> Result<usize> {
            self.timeouts.borrow_mut().push(timeout);
            self.time.set(self.time.get() + self.step.get());
            let report = self
                .reads
                .borrow_mut()
                .pop_front()
                .unwrap_or(Ok(Vec::new()))?;
            buffer[..report.len()].copy_from_slice(&report);
            Ok(report.len())
        }
        fn write(&self, report: &[u8]) -> Result<usize> {
            if self.fail_write.get() {
                bail!("private transport error");
            }
            self.writes.borrow_mut().push(report.to_vec());
            Ok(if self.short_write.get() {
                report.len() - 1
            } else {
                report.len()
            })
        }
    }

    fn report(text: &str) -> Vec<u8> {
        assert!(text.len() <= MAX_CHUNK);
        let mut report = vec![0; REPORT_SIZE];
        report[0] = REPORT_ID;
        report[1] = CHANNEL_RPC;
        report[2] = text.len() as u8;
        report[3..3 + text.len()].copy_from_slice(text.as_bytes());
        report
    }

    fn with_io(reports: Vec<Result<Vec<u8>>>) -> (CreatorMicroDisplay, Rc<FakeIo>) {
        let mut display = configured();
        let time = Rc::new(Cell::new(Instant::now()));
        let io = Rc::new(FakeIo {
            reads: RefCell::new(reports.into()),
            writes: RefCell::new(Vec::new()),
            timeouts: RefCell::new(Vec::new()),
            time: time.clone(),
            step: Cell::new(Duration::ZERO),
            fail_write: Cell::new(false),
            short_write: Cell::new(false),
        });
        display.clock = Rc::new(move || time.get());
        display.pad = Some(Pad {
            dev: Box::new(io.clone()),
            next_id: 1,
            actor_ids: None,
            acc: String::new(),
            acc_times: VecDeque::new(),
            pending: VecDeque::new(),
            clock: display.clock.clone(),
            last_input_success: None,
            last_output_success: None,
            probe_stop: None,
        });
        // A prior authoritative status selected this layer; only raw I/O below is under test.
        display.layer(1, 3);
        display.reset_pending = false;
        (display, io)
    }

    fn budget(io: &FakeIo, raw: usize) -> InputPollBudget {
        InputPollBudget {
            deadline: io.time.get() + Duration::from_millis(5),
            max_raw_frames: raw,
        }
    }

    #[test]
    fn creator_review_malformed_rpc_preserves_release_then_typed_loss() {
        for prefix in [false, true] {
            let mut reports = Vec::new();
            if prefix {
                reports.push(Ok(report(
                    "{\"m\":\"v.oai.hid\",\"p\":{\"act\":0,\"k\":\"AG16\"}}",
                )));
            }
            reports.push(Ok(report("{oops}")));
            reports.push(Ok(Vec::new()));
            let (mut display, io) = with_io(reports);
            let b = budget(&io, 128);
            let batch = display
                .poll_input_batch(InputPollMode::DrainOnly, b)
                .unwrap();
            assert!(
                !batch.is_trusted_drain(b, io.time.get()),
                "malformed RPC is not continuity"
            );
            assert_eq!(batch.continuity, InputContinuity::Lost);
            assert!(!batch.drained);
            assert_eq!(batch.raw_frames, if prefix { 2 } else { 1 });
            if prefix {
                assert!(matches!(
                    &batch.events[..],
                    [
                        DeviceEvent::Key { pressed: false, .. },
                        DeviceEvent::Reset { .. }
                    ]
                ));
            } else {
                assert!(matches!(&batch.events[..], [DeviceEvent::Reset { .. }]));
            }
            let observation = display.diagnostics();
            assert_eq!(observation.connectivity, Connectivity::Disconnected);
            assert_eq!(
                observation.last_failure.unwrap().code,
                DiagnosticCode::InputFailed
            );
            assert!(!format!("{:?}", display.diagnostics()).contains("oops"));
            assert!(io.writes.borrow().is_empty());
        }
        // Ignored valid RPCs/notifications and unrelated HID channels are not loss.
        let (mut display, io) = with_io(vec![
            Ok(report("{\"id\":7,\"result\":{\"ok\":1}}")),
            Ok(report("{\"m\":\"unrelated\",\"p\":{}}")),
            Ok(vec![REPORT_ID, 0x03, 1, 42]),
        ]);
        let b = budget(&io, 128);
        let clean = display
            .poll_input_batch(InputPollMode::DrainOnly, b)
            .unwrap();
        assert!(clean.is_trusted_drain(b, io.time.get()));
        assert_eq!(clean.raw_frames, 3);
        assert!(clean.events.is_empty());
        let (mut display, io) = with_io(vec![Ok(report("{\"m\":\"kb.pl\","))]);
        let incomplete = display
            .poll_input_batch(InputPollMode::DrainOnly, budget(&io, 128))
            .unwrap();
        assert!(incomplete.partial_frame);
        assert!(!incomplete.drained);
        assert_eq!(incomplete.continuity, InputContinuity::Continuous);
        assert_eq!(display.diagnostics().last_failure, None);
    }

    #[test]
    fn creator_review_delayed_selectors_keep_original_read_evidence() {
        for mode in ["buffered", "fragment", "pending"] {
            let reports = match mode {
                "buffered" => vec![Ok(report(
                    "{\"id\":9}{\"m\":\"kb.pl\",\"p\":{\"p\":1,\"l\":6}}",
                ))],
                "fragment" => vec![
                    Ok(report("{\"id\":9}{\"m\":\"kb.pl\",")),
                    Ok(report("\"p\":{\"p\":1,\"l\":6}}")),
                ],
                _ => vec![
                    Ok(report("{\"m\":\"kb.pl\",\"p\":{\"p\":1,\"l\":6}}")),
                    Ok(report("{\"id\":1,\"result\":{}}")),
                ],
            };
            let (mut display, io) = with_io(reports);
            let start = io.time.get();
            if mode == "pending" {
                io.step.set(Duration::from_millis(1));
                display
                    .pad
                    .as_mut()
                    .unwrap()
                    .call_json("test", json!(null))
                    .unwrap();
                io.step.set(Duration::ZERO);
            } else {
                display
                    .poll_input_batch(InputPollMode::DrainOnly, budget(&io, 1))
                    .unwrap();
            }
            let mut expected = if mode == "pending" {
                start + Duration::from_millis(1)
            } else {
                start
            };
            if mode == "fragment" {
                io.time.set(start + Duration::from_millis(30));
                // Complete the old fragment alongside a second buffered selector.
                io.reads.borrow_mut().clear();
                io.reads.borrow_mut().push_back(Ok(report(
                    "\"p\":{\"p\":1,\"l\":6}}{\"m\":\"kb.pl\",\"p\":{\"p\":1,\"l\":6}}",
                )));
                display
                    .poll_input_batch(InputPollMode::DrainOnly, budget(&io, 1))
                    .unwrap();
                expected = io.time.get();
            }
            io.time.set(start + Duration::from_millis(100));
            let delayed = display
                .poll_input_batch(InputPollMode::DrainOnly, budget(&io, 128))
                .unwrap();
            assert_eq!(delayed.raw_frames, 0);
            let old = display.diagnostics();
            assert_eq!(old.layer.as_ref().unwrap().observed_at, expected, "{mode}");
            assert!(
                old.observed_at.unwrap() <= start + Duration::from_millis(30),
                "cached connection not re-dated: {mode}"
            );
            let calls = io.timeouts.borrow().len();
            assert_eq!(display.diagnostics(), old);
            assert_eq!(io.timeouts.borrow().len(), calls);
            // A genuinely new duplicate selector DOES refresh read/layer evidence.
            io.reads
                .borrow_mut()
                .push_back(Ok(report("{\"m\":\"kb.pl\",\"p\":{\"p\":1,\"l\":6}}")));
            display
                .poll_input_batch(InputPollMode::DrainOnly, budget(&io, 128))
                .unwrap();
            let fresh = display.diagnostics();
            assert_eq!(fresh.layer.unwrap().observed_at, io.time.get());
            assert_eq!(fresh.observed_at, Some(io.time.get()));
            assert_eq!(fresh.last_input_success, Some(io.time.get()));
        }
    }

    #[test]
    fn creator_review_status_send_failures_are_output_failures() {
        for short in [false, true] {
            let (mut initial, io) = with_io(Vec::new());
            io.short_write.set(short);
            io.fail_write.set(!short);
            let pad = initial.pad.take().unwrap();
            let mut display = configured();
            display.clock = initial.clock.clone();
            assert!(display.connect_with(|| Ok(pad)).is_err());
            let observation = display.diagnostics();
            assert_eq!(
                observation.last_failure.unwrap().code,
                DiagnosticCode::OutputFailed
            );
            assert_eq!(observation.last_output_success, None);
            assert_eq!(observation.last_input_success, None);
            assert_eq!(observation.layer, None);
            assert!(io.timeouts.borrow().is_empty());
            assert!(!format!("{:?}", display.diagnostics()).contains("private transport"));
        }
        let (mut initial, io) = with_io(vec![Err(anyhow::anyhow!("private read error"))]);
        let pad = initial.pad.take().unwrap();
        let mut display = configured();
        display.clock = initial.clock.clone();
        assert!(display.connect_with(|| Ok(pad)).is_err());
        assert_eq!(
            display.diagnostics().last_failure.unwrap().code,
            DiagnosticCode::InputFailed
        );
        assert_eq!(
            display.diagnostics().last_output_success,
            Some(io.time.get())
        );
    }

    #[test]
    fn creator_review_open_outcomes_are_typed_not_error_text() {
        let mut absent = configured();
        assert!(
            !absent
                .connect_with(|| Err(TransportContext::ExpectedAbsence.into()))
                .unwrap()
        );
        let observation = absent.diagnostics();
        assert_eq!(observation.connectivity, Connectivity::Disconnected);
        let failure = observation.last_failure.unwrap();
        assert_eq!(failure.code, DiagnosticCode::DeviceUnavailable);
        assert!(!failure.active, "expected absence is informational");
        assert_eq!(failure.recovery, Recovery::None);
        for message in [
            "private permission error",
            "no Creator Micro 2 on the HID bus (303A:8297 or 303A:8298)",
        ] {
            let mut opaque = configured();
            assert!(
                !opaque
                    .connect_with(|| Err(anyhow::anyhow!(message)))
                    .unwrap()
            );
            let observation = opaque.diagnostics();
            assert_eq!(observation.connectivity, Connectivity::Unknown);
            assert_eq!(
                observation.last_failure, None,
                "opaque error is not proven absence/permissions"
            );
            assert_eq!(observation.layer, None);
            assert_eq!(observation.last_input_success, None);
            assert_eq!(observation.last_output_success, None);
            assert!(!format!("{observation:?}").contains(message));
        }
    }

    #[test]
    fn creator_drain_counts_partial_reports_and_requires_a_real_empty_read() {
        let (mut display, io) = with_io(vec![
            Ok(report("{\"m\":\"v.oai.hid\",")),
            Ok(report("\"p\":{\"act\":1,\"k\":\"AG16\"}}")),
        ]);
        let first = display
            .poll_input_batch(InputPollMode::DrainOnly, budget(&io, 1))
            .unwrap();
        assert_eq!(first.raw_frames, 1);
        assert!(first.partial_frame);
        assert!(!first.drained);
        assert!(first.events.is_empty());
        let second = display
            .poll_input_batch(InputPollMode::DrainOnly, budget(&io, 1))
            .unwrap();
        assert_eq!(second.raw_frames, 1);
        assert!(!second.partial_frame);
        assert!(!second.drained);
        assert_eq!(
            second.events,
            vec![DeviceEvent::Key {
                address: Address::try_from("creator-micro:rex/16".to_string()).unwrap(),
                pressed: true
            }]
        );
        let b = budget(&io, 128);
        let third = display
            .poll_input_batch(InputPollMode::DrainOnly, b)
            .unwrap();
        assert!(third.is_trusted_drain(b, io.time.get()));
        assert_eq!(third.raw_frames, 0);
        assert_eq!(*io.timeouts.borrow(), [0, 0, 0]);
        assert!(
            io.writes.borrow().is_empty(),
            "DrainOnly must never issue RPCs"
        );
        let reads = io.timeouts.borrow().len();
        let observation = display.diagnostics();
        assert_eq!(observation.last_input_success, Some(io.time.get()));
        assert_eq!(display.diagnostics(), observation);
        assert_eq!(io.timeouts.borrow().len(), reads);
    }

    #[test]
    fn creator_drain_loss_preserves_prior_edges_then_reset_and_typed_history() {
        use crate::display::{Connectivity, DiagnosticCode};
        let (mut display, io) = with_io(vec![
            Ok(report(
                "{\"m\":\"v.oai.hid\",\"p\":{\"act\":0,\"k\":\"AG16\"}}",
            )),
            Err(anyhow::anyhow!("private input path")),
        ]);
        let batch = display
            .poll_input_batch(InputPollMode::DrainOnly, budget(&io, 128))
            .unwrap();
        assert_eq!(batch.raw_frames, 1);
        assert_eq!(batch.continuity, InputContinuity::Lost);
        assert!(!batch.drained);
        assert!(matches!(
            &batch.events[..],
            [
                DeviceEvent::Key { pressed: false, .. },
                DeviceEvent::Reset { .. }
            ]
        ));
        let observation = display.diagnostics();
        assert_eq!(observation.connectivity, Connectivity::Disconnected);
        assert_eq!(observation.layer, None);
        assert_eq!(observation.last_input_success, Some(io.time.get()));
        let failure = observation.last_failure.as_ref().unwrap();
        assert_eq!(failure.code, DiagnosticCode::InputFailed);
        assert!(failure.active);
        assert!(!format!("{observation:?}").contains("private input"));
        let reads = io.timeouts.borrow().len();
        let next = display
            .poll_input_batch(InputPollMode::DrainOnly, budget(&io, 128))
            .unwrap();
        assert!(!next.drained);
        assert_eq!(
            io.timeouts.borrow().len(),
            reads,
            "DrainOnly must not reconnect"
        );
    }

    #[test]
    fn creator_drain_overrun_or_expired_budget_never_claims_exhaustion() {
        let (mut display, io) = with_io(vec![Ok(Vec::new())]);
        io.step.set(Duration::from_millis(6));
        let b = budget(&io, 128);
        let batch = display
            .poll_input_batch(InputPollMode::DrainOnly, b)
            .unwrap();
        assert!(
            !batch.drained,
            "an empty read completing after the shared deadline is not a drain fence"
        );
        assert!(!batch.is_trusted_drain(b, io.time.get()));
        assert_eq!(batch.raw_frames, 0);
        assert_eq!(display.diagnostics().last_input_success, None);
        let calls = io.timeouts.borrow().len();
        let expired = InputPollBudget {
            deadline: io.time.get(),
            max_raw_frames: 128,
        };
        assert!(
            !display
                .poll_input_batch(InputPollMode::DrainOnly, expired)
                .unwrap()
                .drained
        );
        assert!(
            !display
                .poll_input_batch(InputPollMode::DrainOnly, budget(&io, 0))
                .unwrap()
                .drained
        );
        assert_eq!(io.timeouts.borrow().len(), calls);
    }

    #[test]
    fn creator_drain_frame_129_reset_precedes_release_and_needs_a_new_fence() {
        let mut reports = (0..128)
            .map(|_| Ok(report("{\"id\":42,\"result\":{\"ok\":1}}")))
            .collect::<Vec<_>>();
        reports.push(Ok(report("{\"m\":\"kb.pl\",\"p\":{\"p\":1,\"l\":6}}")));
        reports.push(Ok(report(
            "{\"m\":\"v.oai.hid\",\"p\":{\"act\":0,\"k\":\"AG16\"}}",
        )));
        let (mut display, io) = with_io(reports);
        let b = budget(&io, 128);
        let first = display
            .poll_input_batch(InputPollMode::DrainOnly, b)
            .unwrap();
        assert_eq!(first.raw_frames, 128);
        assert!(!first.is_trusted_drain(b, io.time.get()));
        assert!(!first.drained);
        assert!(first.events.is_empty());
        assert_eq!(io.reads.borrow().len(), 2);
        let second = display
            .poll_input_batch(InputPollMode::DrainOnly, b)
            .unwrap();
        assert_eq!(second.raw_frames, 2);
        assert!(second.drained);
        assert_eq!(second.continuity, InputContinuity::Reset);
        assert!(matches!(
            &second.events[..],
            [
                DeviceEvent::Reset { .. },
                DeviceEvent::Key { pressed: false, .. }
            ]
        ));
        assert!(!second.is_trusted_drain(b, io.time.get()));
        let layer = display.diagnostics().layer.unwrap();
        assert_eq!(
            (layer.name.as_deref(), layer.position),
            (Some("test"), Some(6))
        );
        assert!(
            display
                .poll_input_batch(InputPollMode::DrainOnly, b)
                .unwrap()
                .is_trusted_drain(b, io.time.get())
        );
        assert!(io.writes.borrow().is_empty());
    }

    #[test]
    fn creator_drain_partial_then_would_block_is_still_not_exhausted() {
        let (mut display, io) = with_io(vec![Ok(report("{\"m\":\"kb.pl\","))]);
        let b = budget(&io, 128);
        let first = display
            .poll_input_batch(InputPollMode::DrainOnly, b)
            .unwrap();
        assert_eq!(first.raw_frames, 1);
        assert!(first.partial_frame);
        assert!(!first.drained);
        let at = display.diagnostics().last_input_success;
        io.time.set(io.time.get() + Duration::from_millis(2));
        let second = display
            .poll_input_batch(InputPollMode::DrainOnly, budget(&io, 128))
            .unwrap();
        assert!(second.partial_frame);
        assert!(!second.drained);
        assert_eq!(display.diagnostics().last_input_success, at);
        io.reads
            .borrow_mut()
            .push_back(Ok(report("\"p\":{\"p\":1,\"l\":2}}")));
        let third = display
            .poll_input_batch(InputPollMode::DrainOnly, budget(&io, 128))
            .unwrap();
        assert!(!third.partial_frame);
        assert_eq!(third.continuity, InputContinuity::Reset);
        assert_eq!(display.diagnostics().layer.unwrap().name, None);

        let (mut interrupted, io) = with_io(vec![
            Ok(report("{\"m\":\"kb.pl\",")),
            Err(anyhow::anyhow!("lost during fragment")),
        ]);
        let lost = interrupted
            .poll_input_batch(InputPollMode::DrainOnly, budget(&io, 128))
            .unwrap();
        assert_eq!(lost.continuity, InputContinuity::Lost);
        assert!(
            lost.partial_frame,
            "loss must not disguise the retained incomplete JSON"
        );
        assert!(!lost.drained);
    }

    #[test]
    fn creator_drain_loss_does_not_close_a_synchronous_hid_handle() {
        struct TrackedIo(Rc<FakeIo>, Rc<Cell<usize>>);
        impl ReportIo for TrackedIo {
            fn write(&self, report: &[u8]) -> Result<usize> {
                self.0.write(report)
            }
            fn read_timeout(&self, report: &mut [u8], timeout: i32) -> Result<usize> {
                self.0.read_timeout(report, timeout)
            }
        }
        impl Drop for TrackedIo {
            fn drop(&mut self) {
                self.1.set(self.1.get() + 1);
            }
        }
        let (mut display, io) = with_io(vec![Err(anyhow::anyhow!("unplugged"))]);
        let drops = Rc::new(Cell::new(0));
        display.pad.as_mut().unwrap().dev = Box::new(TrackedIo(io.clone(), drops.clone()));
        let batch = display
            .poll_input_batch(InputPollMode::DrainOnly, budget(&io, 128))
            .unwrap();
        assert_eq!(batch.continuity, InputContinuity::Lost);
        assert_eq!(
            drops.get(),
            0,
            "hid_close can join a thread; do not run it in DrainOnly"
        );
        drop(display);
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn creator_output_observation_is_a_host_send_not_an_ack_or_refresh_read() {
        let (mut display, io) = with_io(Vec::new());
        let plan = crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap();
        let at = io.time.get();
        assert_eq!(display.publish(&plan).unwrap(), PublishOutcome::Updated);
        assert_eq!(display.diagnostics().last_output_success, Some(at));
        assert_eq!(
            display.diagnostics().last_input_success,
            None,
            "writes must not wait for an ACK"
        );
        assert!(io.timeouts.borrow().is_empty());
        let reports = io.writes.borrow().len();
        io.time.set(at + Duration::from_millis(100));
        assert_eq!(display.publish(&plan).unwrap(), PublishOutcome::Unchanged);
        assert_eq!(io.writes.borrow().len(), reports);
        assert_eq!(display.diagnostics().last_output_success, Some(at));
        io.time.set(at + Duration::from_millis(500));
        io.fail_write.set(true);
        assert!(display.publish(&plan).is_err());
        let observation = display.diagnostics();
        assert_eq!(observation.last_output_success, Some(at));
        assert_eq!(observation.layer, None);
        assert_eq!(observation.connectivity, Connectivity::Disconnected);
        assert_eq!(
            observation.last_failure.unwrap().code,
            DiagnosticCode::OutputFailed
        );
    }

    #[test]
    fn creator_short_write_is_not_recorded_as_a_successful_output() {
        let (mut display, io) = with_io(Vec::new());
        io.short_write.set(true);
        let plan = crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap();
        assert!(display.publish(&plan).is_err());
        let observed = display.diagnostics();
        assert_eq!(observed.last_output_success, None);
        assert_eq!(
            observed.last_failure.unwrap().code,
            DiagnosticCode::OutputFailed
        );
    }

    fn rpc_reports(text: &str) -> Vec<Result<Vec<u8>>> {
        text.as_bytes()
            .chunks(MAX_CHUNK)
            .map(|bytes| Ok(report(std::str::from_utf8(bytes).unwrap())))
            .collect()
    }

    #[test]
    fn creator_legacy_status_progression_records_real_io_and_discards_pre_status_keys() {
        let mut reports = rpc_reports("{\"m\":\"v.oai.hid\",\"p\":{\"act\":1,\"k\":\"AG16\"}}");
        reports.extend(rpc_reports(
            "{\"id\":1,\"result\":{\"profile_index\":1,\"layer_index\":3}}",
        ));
        reports.extend(rpc_reports(
            "{\"m\":\"v.oai.hid\",\"p\":{\"act\":0,\"k\":\"AG16\"}}",
        ));
        let (mut initial, io) = with_io(reports);
        let pad = initial.pad.take().unwrap();
        let mut display = configured();
        display.clock = initial.clock.clone();
        let sent_at = io.time.get();
        io.step.set(Duration::from_millis(1));
        assert!(display.connect_with(|| Ok(pad)).unwrap());
        let observation = display.diagnostics();
        assert_eq!(observation.last_output_success, Some(sent_at));
        assert_eq!(observation.last_input_success, Some(io.time.get()));
        assert_eq!(observation.layer.unwrap().position, Some(3));
        assert_eq!(
            display.poll_input().unwrap(),
            vec![
                DeviceEvent::Reset {
                    device: "creator-micro".into()
                },
                DeviceEvent::Key {
                    address: Address::try_from("creator-micro:rex/16".to_string()).unwrap(),
                    pressed: false
                }
            ]
        );
        let observed_at = display.diagnostics().last_input_success;
        display.poll_input().unwrap();
        assert_eq!(display.diagnostics().last_input_success, observed_at);
    }

    #[test]
    fn creator_reconnect_is_explicitly_deferred_without_bounded_platform_open() {
        use crate::display::ReconnectOutcome;
        let (mut display, io) = with_io(Vec::new());
        let observed = display.diagnostics();
        assert_eq!(
            display
                .reconnect_step(io.time.get() + Duration::from_millis(5))
                .unwrap(),
            ReconnectOutcome::Connected
        );
        assert_eq!(
            display.diagnostics(),
            observed,
            "reusing established state does not refresh evidence"
        );
        assert_eq!(
            display.reconnect_step(io.time.get()).unwrap(),
            ReconnectOutcome::Deferred
        );
        display.lost();
        assert_eq!(
            display
                .reconnect_step(io.time.get() + Duration::from_millis(5))
                .unwrap(),
            ReconnectOutcome::Deferred
        );
        let mut fresh = configured();
        fresh.clock = display.clock.clone();
        assert_eq!(
            fresh
                .reconnect_step(io.time.get() + Duration::from_millis(5))
                .unwrap(),
            ReconnectOutcome::Deferred
        );
        assert_eq!(fresh.diagnostics().connectivity, Connectivity::Unknown);
        assert!(io.timeouts.borrow().is_empty());
        assert!(io.writes.borrow().is_empty());
    }

    #[test]
    fn creator_status_invalid_selector_never_becomes_observed_layer() {
        for result in [
            json!({"profile_index":1,"layer_index":0}),
            json!({"profile_index":1}),
            json!({"profile_index":1,"layer_index":65536}),
            json!({"profile_index":"bad","layer_index":3}),
        ] {
            let reports = rpc_reports(&json!({"id":1,"result":result}).to_string());
            let (mut initial, io) = with_io(reports);
            let pad = initial.pad.take().unwrap();
            let mut display = configured();
            display.clock = initial.clock.clone();
            assert!(
                display.connect_with(|| Ok(pad)).is_err(),
                "invalid selector: {result}"
            );
            assert_eq!(display.diagnostics().layer, None);
            assert_eq!(
                display.diagnostics().connectivity,
                Connectivity::Disconnected
            );
            assert_eq!(
                display.diagnostics().last_input_success,
                Some(io.time.get())
            );
        }
    }

    #[test]
    fn creator_legacy_timeout_open_failure_and_recovery_keep_truthful_history() {
        let (mut initial, io) = with_io(Vec::new());
        let pad = initial.pad.take().unwrap();
        let mut display = configured();
        display.clock = initial.clock.clone();
        let start = io.time.get();
        io.step.set(Duration::from_millis(350));
        assert!(display.connect_with(|| Ok(pad)).is_err());
        let failed = display.diagnostics();
        assert_eq!(failed.layer, None);
        assert_eq!(failed.last_input_success, None);
        assert_eq!(failed.last_output_success, Some(start));
        assert_eq!(
            failed.last_failure.unwrap().code,
            DiagnosticCode::InputFailed
        );
        assert_eq!(*io.timeouts.borrow(), [250, 250]);
        assert!(
            !display
                .connect_with(|| panic!("retry must be deferred for 2 seconds"))
                .unwrap()
        );
        io.time.set(start + Duration::from_secs(2));
        assert!(
            !display
                .connect_with(|| bail!("private permission/error text"))
                .unwrap()
        );
        let failed = display.diagnostics();
        assert_eq!(failed.connectivity, Connectivity::Unknown);
        assert_eq!(
            failed.last_failure.unwrap().code,
            DiagnosticCode::InputFailed
        );
        assert!(!format!("{:?}", display.diagnostics()).contains("private permission"));
        io.step.set(Duration::ZERO);
        io.time.set(start + Duration::from_secs(4));
        io.reads.borrow_mut().extend(rpc_reports(
            "{\"id\":1,\"result\":{\"profile_index\":1,\"layer_index\":2}}",
        ));
        let pad = Pad {
            dev: Box::new(io.clone()),
            next_id: 1,
            actor_ids: None,
            acc: String::new(),
            acc_times: VecDeque::new(),
            pending: VecDeque::new(),
            clock: display.clock.clone(),
            last_input_success: None,
            last_output_success: None,
            probe_stop: None,
        };
        assert!(display.connect_with(|| Ok(pad)).unwrap());
        let recovered = display.diagnostics();
        assert_eq!(recovered.connectivity, Connectivity::Connected);
        assert_eq!(recovered.layer.unwrap().name, None);
        assert!(!recovered.last_failure.unwrap().active);
        assert_eq!(recovered.last_input_success, Some(io.time.get()));
    }

    pub(super) fn configured() -> CreatorMicroDisplay {
        CreatorMicroDisplay::new(DeviceConfig {
            id: "creator-micro".into(),
            path: "creator-micro.toml".into(),
            definition: toml::from_str(include_str!("../templates/creator-micro.toml")).unwrap(),
        })
        .unwrap()
    }

    #[test]
    fn creator_observation_starts_unknown_with_configuration_not_hardware_evidence() {
        use crate::display::Connectivity;
        let display = configured();
        let observation = display.diagnostics();
        assert!(observation.observation_supported);
        assert_eq!(observation.connectivity, Connectivity::Unknown);
        assert_eq!(observation.observed_at, None);
        assert_eq!(observation.layer, None);
        assert_eq!(observation.last_input_success, None);
        assert_eq!(observation.last_output_success, None);
        assert_eq!(observation.last_failure, None);
        assert_eq!(observation.member_descriptors.len(), 20);
        let member = observation
            .member_descriptors
            .iter()
            .find(|member| member.address.address == "rex/6")
            .unwrap();
        assert_eq!(member.address.device, "creator-micro");
        assert_eq!(member.name, None);
        let layer = member.layer.as_ref().unwrap();
        assert_eq!(layer.name, "rex");
        assert_eq!((layer.profile, layer.position), (Some(1), Some(3)));
        assert_eq!(display.diagnostics(), observation);
    }

    #[test]
    fn creator_selector_observation_survives_engine_reset_but_not_loss() {
        use crate::display::Connectivity;
        use std::{cell::Cell, rc::Rc};
        let mut display = configured();
        let at = Instant::now();
        let time = Rc::new(Cell::new(at));
        let clock = time.clone();
        display.clock = Rc::new(move || clock.get());
        assert!(matches!(
            display.decode(&json!({"m":"kb.pl","p":{"p":1,"l":3}})),
            Some(DeviceEvent::Reset { .. })
        ));
        let observed = display.diagnostics();
        assert_eq!(observed.connectivity, Connectivity::Connected);
        let layer = observed.layer.unwrap();
        assert_eq!(layer.name.as_deref(), Some("rex"));
        assert_eq!(
            (layer.profile, layer.position, layer.observed_at),
            (Some(1), Some(3), at)
        );
        time.set(at + Duration::from_millis(3));
        assert!(
            display
                .decode(&json!({"m":"kb.pl","p":{"p":1,"l":3}}))
                .is_none()
        );
        assert_eq!(display.diagnostics().layer.unwrap().observed_at, time.get());
        display.decode(&json!({"m":"kb.pl","p":{"p":1,"l":2}}));
        let native = display.diagnostics().layer.unwrap();
        assert_eq!(native.name, None);
        assert_eq!(native.position, Some(2));
        display.decode(&json!({"m":"kb.pl","p":{"p":1,"l":"bad"}}));
        assert_eq!(display.diagnostics().layer, Some(native));
        display.lost();
        let lost = display.diagnostics();
        assert_eq!(lost.connectivity, Connectivity::Disconnected);
        assert_eq!(lost.layer, None);
        assert!(display.reset_pending);
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
        let mut device = configured().device.clone();
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
        let mut device = configured().device.clone();
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
            device.definition.actions.get_mut("dictate").unwrap()
        else {
            panic!("expected legacy chord fixture");
        };
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
        assert!(first.unwrap().is_none());
        acc.push_str("\"p\":{\"k\":\"AG06\",\"act\":1}}");
        let value = take_json(&mut acc).unwrap().unwrap();
        assert_eq!(value["p"]["k"], "AG06");
        assert!(acc.is_empty());
    }

    #[test]
    fn concatenated_frames_keep_every_edge_in_wire_order() {
        let mut acc = String::from(
            r#"{"m":"v.oai.hid","p":{"k":"AG16","act":1}}{"m":"v.oai.hid","p":{"k":"AG16","act":0}}"#,
        );
        assert_eq!(take_json(&mut acc).unwrap().unwrap()["p"]["act"], 1);
        assert_eq!(take_json(&mut acc).unwrap().unwrap()["p"]["act"], 0);
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
