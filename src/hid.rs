//! USB-only diagnostic access to the Beckon status transport.
//!
//! This module intentionally has no dependency on the daemon or render plan.
//! It exists to prove the host-to-keyboard protocol before automated status
//! delivery is enabled.

use std::{ffi::CStr, fmt, str::FromStr};

use anyhow::{Context, Result, bail};
use hidapi::{DeviceInfo, HidApi, HidDevice};

pub const VENDOR_USAGE_PAGE: u16 = 0xFF60;
pub const STATUS_USAGE: u16 = 0x0061;
pub const GLOVE80_VENDOR_ID: u16 = 0x16C0;
pub const GLOVE80_PRODUCT_ID: u16 = 0x27DB;
pub const REPORT_SIZE: usize = 32;
pub const SLOT_COUNT: usize = 10;
pub const PROTOCOL_VERSION: u8 = 1;
const SNAPSHOT_MESSAGE_TYPE: u8 = 1;

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
}

impl StatusSnapshot {
    /// Encode a transport-v1 output report. All reserved bytes remain zero.
    pub fn encode(self) -> [u8; REPORT_SIZE] {
        let mut report = [0; REPORT_SIZE];
        report[0] = PROTOCOL_VERSION;
        report[1] = SNAPSHOT_MESSAGE_TYPE;
        report[2] = self.sequence;
        for (index, state) in self.slots.into_iter().enumerate() {
            report[4 + index] = state as u8;
        }
        report
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
        [] => bail!(
            "no Beckon USB status endpoint found (expected {GLOVE80_VENDOR_ID:04X}:{GLOVE80_PRODUCT_ID:04X}, usage page 0x{VENDOR_USAGE_PAGE:04X}, usage 0x{STATUS_USAGE:04X})"
        ),
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
        [] => bail!(
            "no Beckon USB status endpoint found (expected {GLOVE80_VENDOR_ID:04X}:{GLOVE80_PRODUCT_ID:04X}, usage page 0x{VENDOR_USAGE_PAGE:04X}, usage 0x{STATUS_USAGE:04X})"
        ),
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

/// Write one complete, valid v1 snapshot. This function never derives state
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
    use super::*;

    #[test]
    fn encodes_complete_snapshot_with_zeroed_reserved_bytes() {
        let report = StatusSnapshot {
            sequence: 42,
            slots: [
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
        }
        .encode();

        assert_eq!(report.len(), REPORT_SIZE);
        assert_eq!(&report[..4], &[1, 1, 42, 0]);
        assert_eq!(&report[4..14], &[0, 1, 2, 3, 4, 5, 0, 0, 0, 0]);
        assert!(report[14..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn parses_only_protocol_status_names() {
        assert_eq!("working".parse::<Status>(), Ok(Status::Working));
        assert!("paused".parse::<Status>().is_err());
    }
}
