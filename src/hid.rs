//! USB-only Beckon status transport and its explicit diagnostic commands.
//!
//! The daemon depends only on the narrow [`StatusWriter`] boundary; manual
//! protocol diagnostics remain explicit CLI actions.

use std::{
    collections::BTreeMap,
    error::Error,
    ffi::CStr,
    fmt,
    str::FromStr,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use hidapi::{DeviceInfo, HidApi, HidDevice};

use crate::{
    core::KEY_IDS,
    render::{AgentState, Motion, RenderPlan, Rgb, StateTreatment},
};

pub const VENDOR_USAGE_PAGE: u16 = 0xFF60;
pub const STATUS_USAGE: u16 = 0x0061;
pub const GLOVE80_VENDOR_ID: u16 = 0x16C0;
pub const GLOVE80_PRODUCT_ID: u16 = 0x27DB;
pub const REPORT_SIZE: usize = 32;
pub const SLOT_COUNT: usize = 10;
pub const PROTOCOL_VERSION: u8 = 2;
const SNAPSHOT_MESSAGE_TYPE: u8 = 1;

/// Expected device-lifecycle condition used by optional display adapters.
///
/// Keeping this typed lets the daemon distinguish an unplugged keyboard from
/// malformed reports and HID failures without matching human-readable text.
#[derive(Debug)]
pub(crate) struct StatusEndpointUnavailable;

impl fmt::Display for StatusEndpointUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "no Beckon USB status endpoint found (expected {GLOVE80_VENDOR_ID:04X}:{GLOVE80_PRODUCT_ID:04X}, usage page 0x{VENDOR_USAGE_PAGE:04X}, usage 0x{STATUS_USAGE:04X})"
        )
    }
}

impl Error for StatusEndpointUnavailable {}

pub(crate) fn is_status_endpoint_unavailable(error: &anyhow::Error) -> bool {
    error.downcast_ref::<StatusEndpointUnavailable>().is_some()
}

/// A state value defined by Beckon status transport v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    Unbound = 0,
    Idle = 1,
    Working = 2,
    Blocked = 3,
    Done = 4,
    Unknown = 5,
}

impl fmt::Display for Status {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unbound => "unbound",
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Unknown => "unknown",
        })
    }
}

impl FromStr for Status {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "unbound" => Ok(Self::Unbound),
            "idle" => Ok(Self::Idle),
            "working" => Ok(Self::Working),
            "blocked" => Ok(Self::Blocked),
            "done" => Ok(Self::Done),
            "unknown" => Ok(Self::Unknown),
            _ => Err(format!(
                "invalid status {value:?}; expected unbound, idle, working, blocked, done, or unknown"
            )),
        }
    }
}

/// A complete, atomic state view of the ten Beckon keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusSnapshot {
    pub sequence: u8,
    pub slots: [Status; SLOT_COUNT],
    pub treatments: [Treatment; 5],
}

/// A host-resolved treatment. Named themes stay on the host; this is the
/// compact, keyboard-agnostic data that firmware needs to render them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Treatment {
    pub color: Rgb,
    pub brightness: u8,
    pub motion: Motion,
}

/// The narrow output boundary used by the daemon. Keeping it small lets the
/// state-to-transport policy be tested without a physical HID device.
pub trait StatusWriter {
    fn write_snapshot(&mut self, snapshot: StatusSnapshot) -> Result<()>;
}

/// The real USB writer. It opens the strict Glove80 endpoint for every state
/// change, which makes unplug/replug recovery an ordinary subsequent write.
#[derive(Default)]
pub struct UsbStatusWriter;

impl StatusWriter for UsbStatusWriter {
    fn write_snapshot(&mut self, snapshot: StatusSnapshot) -> Result<()> {
        send(snapshot)
    }
}

/// Convert hardware-neutral render plans into protocol snapshots and avoid
/// rewriting the keyboard until its ten meaningful states change.
///
/// A failed write intentionally does not update `last_slots`, so a later
/// render pass can recover after a keyboard reconnects. Retrying immediately
/// is prohibitively expensive on macOS: opening hidapi enumerates every HID
/// device. Keep disconnect recovery responsive without turning an unplugged
/// keyboard into a busy loop.
pub struct RenderSink<W> {
    writer: W,
    last_snapshot: Option<([Status; SLOT_COUNT], [Treatment; 5])>,
    next_sequence: u8,
    retry_after: Option<Instant>,
}

const DISCONNECTED_RETRY_DELAY: Duration = Duration::from_secs(2);

impl<W> RenderSink<W>
where
    W: StatusWriter,
{
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            last_snapshot: None,
            next_sequence: 0,
            retry_after: None,
        }
    }

    /// Returns whether a USB snapshot was written.
    pub fn publish(&mut self, plan: &RenderPlan) -> Result<bool> {
        self.publish_at(plan, Instant::now())
    }

    fn publish_at(&mut self, plan: &RenderPlan, now: Instant) -> Result<bool> {
        let slots = slots_for_plan(plan)?;
        let treatments = treatments_for_plan(plan);
        if self.last_snapshot == Some((slots, treatments)) {
            return Ok(false);
        }
        if self
            .retry_after
            .is_some_and(|retry_after| now < retry_after)
        {
            return Ok(false);
        }
        let snapshot = StatusSnapshot {
            sequence: self.next_sequence,
            slots,
            treatments,
        };
        if let Err(error) = self.writer.write_snapshot(snapshot) {
            self.retry_after = Some(now + DISCONNECTED_RETRY_DELAY);
            return Err(error);
        }
        self.last_snapshot = Some((slots, treatments));
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.retry_after = None;
        Ok(true)
    }

    pub fn into_inner(self) -> W {
        self.writer
    }
}

fn treatments_for_plan(plan: &RenderPlan) -> [Treatment; 5] {
    plan.treatments.ordered().map(treatment_for_render)
}

fn treatment_for_render(treatment: StateTreatment) -> Treatment {
    Treatment {
        color: treatment.color,
        brightness: (treatment.brightness.clamp(0.0, 1.0) * 255.0).round() as u8,
        motion: treatment.motion,
    }
}

fn slots_for_plan(plan: &RenderPlan) -> Result<[Status; SLOT_COUNT]> {
    let renders = plan
        .keys
        .iter()
        .map(|render| (render.key.as_str(), render))
        .collect::<BTreeMap<_, _>>();
    if plan.keys.len() != SLOT_COUNT || renders.len() != SLOT_COUNT {
        bail!("render plan must contain exactly {SLOT_COUNT} Beckon keys");
    }
    let mut slots = [Status::Unbound; SLOT_COUNT];
    for (index, key) in KEY_IDS.into_iter().enumerate() {
        let render = renders
            .get(key)
            .with_context(|| format!("render plan omitted {key}"))?;
        slots[index] = status_for_state(render.state);
    }
    Ok(slots)
}

fn status_for_state(state: Option<AgentState>) -> Status {
    match state {
        None => Status::Unbound,
        Some(AgentState::Idle) => Status::Idle,
        Some(AgentState::Working) => Status::Working,
        Some(AgentState::Blocked) => Status::Blocked,
        Some(AgentState::Done) => Status::Done,
        Some(AgentState::Unknown) => Status::Unknown,
    }
}

impl StatusSnapshot {
    /// Produce a valid v2 snapshot for explicit CLI diagnostics. The daemon
    /// always replaces these defaults with its selected configuration theme.
    pub fn for_manual_send(sequence: u8, slots: [Status; SLOT_COUNT]) -> Self {
        Self {
            sequence,
            slots,
            treatments: [
                Treatment {
                    color: Rgb {
                        red: 59,
                        green: 160,
                        blue: 255,
                    },
                    brightness: 51,
                    motion: Motion::Steady,
                },
                Treatment {
                    color: Rgb {
                        red: 249,
                        green: 226,
                        blue: 175,
                    },
                    brightness: 153,
                    motion: Motion::Breathe,
                },
                Treatment {
                    color: Rgb {
                        red: 243,
                        green: 139,
                        blue: 168,
                    },
                    brightness: 204,
                    motion: Motion::Pulse,
                },
                Treatment {
                    color: Rgb {
                        red: 166,
                        green: 227,
                        blue: 161,
                    },
                    brightness: 204,
                    motion: Motion::Steady,
                },
                Treatment {
                    color: Rgb {
                        red: 108,
                        green: 112,
                        blue: 134,
                    },
                    brightness: 77,
                    motion: Motion::Flicker,
                },
            ],
        }
    }

    /// Encode a transport-v2 output report. The ten statuses use 3-bit fields
    /// so five RGB treatments, brightnesses, and effects fit in 32 bytes.
    pub fn encode(self) -> [u8; REPORT_SIZE] {
        let mut report = [0; REPORT_SIZE];
        report[0] = PROTOCOL_VERSION;
        report[1] = SNAPSHOT_MESSAGE_TYPE;
        report[2] = self.sequence;
        for (index, state) in self.slots.into_iter().enumerate() {
            pack_three_bits(&mut report, 32 + index * 3, state as u8);
        }
        for (index, treatment) in self.treatments.into_iter().enumerate() {
            let offset = 8 + index * 3;
            report[offset] = treatment.color.red;
            report[offset + 1] = treatment.color.green;
            report[offset + 2] = treatment.color.blue;
            report[23 + index] = treatment.brightness;
            pack_three_bits(
                &mut report,
                28 * 8 + index * 3,
                motion_code(treatment.motion),
            );
        }
        report
    }
}

fn pack_three_bits(report: &mut [u8; REPORT_SIZE], bit: usize, value: u8) {
    let byte = bit / 8;
    let offset = bit % 8;
    report[byte] |= value << offset;
    if offset > 5 {
        report[byte + 1] |= value >> (8 - offset);
    }
}

fn motion_code(motion: Motion) -> u8 {
    match motion {
        Motion::Steady => 0,
        Motion::Breathe => 1,
        Motion::Pulse => 2,
        Motion::Flicker => 3,
    }
}

/// Public, printable description of a matching vendor HID interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub path: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub interface_number: i32,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial_number: Option<String>,
}

impl Endpoint {
    fn from_info(info: &DeviceInfo) -> Self {
        Self {
            path: c_string(info.path()),
            vendor_id: info.vendor_id(),
            product_id: info.product_id(),
            interface_number: info.interface_number(),
            manufacturer: info.manufacturer_string().map(ToOwned::to_owned),
            product: info.product_string().map(ToOwned::to_owned),
            serial_number: info.serial_number().map(ToOwned::to_owned),
        }
    }
}

fn c_string(value: &CStr) -> String {
    value.to_string_lossy().into_owned()
}

fn is_status_endpoint(info: &DeviceInfo) -> bool {
    info.vendor_id() == GLOVE80_VENDOR_ID
        && info.product_id() == GLOVE80_PRODUCT_ID
        && info.usage_page() == VENDOR_USAGE_PAGE
        && info.usage() == STATUS_USAGE
}

pub fn list() -> Result<Vec<Endpoint>> {
    let api = HidApi::new().context("initialize HID access")?;
    Ok(api
        .device_list()
        .filter(|info| is_status_endpoint(info))
        .map(Endpoint::from_info)
        .collect())
}

/// Open exactly one Beckon endpoint. The firmware only exposes this interface
/// through USB, so VID/PID/page/usage selection avoids the normal keyboard HID
/// interfaces and does not infer a device path from a product name.
pub fn open() -> Result<HidDevice> {
    let api = HidApi::new().context("initialize HID access")?;
    let endpoints = api
        .device_list()
        .filter(|info| is_status_endpoint(info))
        .collect::<Vec<_>>();
    match endpoints.as_slice() {
        [] => Err(StatusEndpointUnavailable.into()),
        [endpoint] => api
            .open_path(endpoint.path())
            .context("open Beckon USB status endpoint"),
        _ => bail!(
            "found {} Beckon status endpoints; disconnect other matching keyboards before writing",
            endpoints.len()
        ),
    }
}

/// Ensure the vendor endpoint can be opened without changing keyboard state.
pub fn probe() -> Result<Endpoint> {
    let api = HidApi::new().context("initialize HID access")?;
    let endpoints = api
        .device_list()
        .filter(|info| is_status_endpoint(info))
        .collect::<Vec<_>>();
    match endpoints.as_slice() {
        [] => Err(StatusEndpointUnavailable.into()),
        [endpoint] => {
            api.open_path(endpoint.path())
                .context("open Beckon USB status endpoint")?;
            Ok(Endpoint::from_info(endpoint))
        }
        _ => bail!(
            "found {} Beckon status endpoints; disconnect other matching keyboards before probing",
            endpoints.len()
        ),
    }
}

/// Write one complete, valid v2 snapshot. This function never derives state
/// from Herdr; callers must explicitly construct the payload they intend to
/// test.
pub fn send(snapshot: StatusSnapshot) -> Result<()> {
    let device = open()?;
    write_exact(&device, &snapshot.encode())
}

/// Send a deliberately malformed short report for the physical rejection test.
/// This is never used by daemon code and must remain an explicit CLI action.
pub fn send_malformed() -> Result<()> {
    let device = open()?;
    write_exact(&device, &[PROTOCOL_VERSION; REPORT_SIZE - 1])
}

fn write_exact(device: &HidDevice, report: &[u8]) -> Result<()> {
    let written = device
        .write(report)
        .context("write Beckon USB status report")?;
    if written != report.len() {
        bail!(
            "short Beckon USB status write: wrote {written} of {} bytes",
            report.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::render::{KeyRender, Motion, Rgb};

    #[test]
    fn encodes_complete_snapshot_with_zeroed_reserved_bytes() {
        let report = StatusSnapshot::for_manual_send(
            42,
            [
                Status::Unbound,
                Status::Idle,
                Status::Working,
                Status::Blocked,
                Status::Done,
                Status::Unknown,
                Status::Unbound,
                Status::Unbound,
                Status::Unbound,
                Status::Unbound,
            ],
        )
        .encode();

        assert_eq!(report.len(), REPORT_SIZE);
        assert_eq!(&report[..4], &[2, 1, 42, 0]);
        assert_eq!(&report[4..8], &[0b10001000, 0b11000110, 0b00000010, 0]);
        assert_eq!(&report[8..11], &[59, 160, 255]);
        assert_eq!(&report[23..28], &[51, 153, 204, 204, 77]);
        assert_eq!(&report[28..30], &[0b10001000, 0b00110000]);
        assert!(report[30..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn parses_only_protocol_status_names() {
        assert_eq!("working".parse::<Status>(), Ok(Status::Working));
        assert!("paused".parse::<Status>().is_err());
    }

    #[derive(Default)]
    struct RecordingWriter {
        writes: Vec<StatusSnapshot>,
        failures: VecDeque<bool>,
    }

    impl StatusWriter for RecordingWriter {
        fn write_snapshot(&mut self, snapshot: StatusSnapshot) -> Result<()> {
            if self.failures.pop_front().unwrap_or(false) {
                bail!("simulated disconnected keyboard");
            }
            self.writes.push(snapshot);
            Ok(())
        }
    }

    fn plan(states: [Option<AgentState>; SLOT_COUNT]) -> RenderPlan {
        RenderPlan {
            keys: KEY_IDS
                .into_iter()
                .zip(states)
                .map(|(key, state)| KeyRender {
                    key: key.into(),
                    state,
                    color: "#000000".parse::<Rgb>().unwrap(),
                    brightness: 0.0,
                    motion: Motion::Steady,
                })
                .collect(),
            treatments: crate::render::DisplayConfig::default()
                .selected_theme()
                .unwrap(),
        }
    }

    #[test]
    fn render_sink_translates_and_deduplicates_meaningful_states() {
        let mut sink = RenderSink::new(RecordingWriter::default());
        let working = plan([
            Some(AgentState::Working),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ]);

        assert!(sink.publish(&working).unwrap());
        assert!(!sink.publish(&working).unwrap());

        let writer = sink.into_inner();
        assert_eq!(writer.writes.len(), 1);
        assert_eq!(writer.writes[0].sequence, 0);
        assert_eq!(writer.writes[0].slots[0], Status::Working);
        assert!(
            writer.writes[0].slots[1..]
                .iter()
                .all(|status| *status == Status::Unbound)
        );
    }

    #[test]
    fn render_sink_retries_a_snapshot_after_a_disconnect() {
        let writer = RecordingWriter {
            failures: VecDeque::from([true, false]),
            ..Default::default()
        };
        let mut sink = RenderSink::new(writer);
        let idle = plan([
            Some(AgentState::Idle),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ]);

        let now = Instant::now();
        assert!(sink.publish_at(&idle, now).is_err());
        assert!(
            !sink
                .publish_at(
                    &idle,
                    now + DISCONNECTED_RETRY_DELAY - Duration::from_millis(1)
                )
                .unwrap()
        );
        assert!(
            sink.publish_at(&idle, now + DISCONNECTED_RETRY_DELAY)
                .unwrap()
        );

        let writer = sink.into_inner();
        assert_eq!(writer.writes.len(), 1);
        assert_eq!(writer.writes[0].sequence, 0);
    }
}
