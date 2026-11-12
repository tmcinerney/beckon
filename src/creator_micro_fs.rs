//! Device filesystem transport for the Creator Micro 2.
//!
//! `fs.txbegin` / `fs.writebin` / `fs.txcommit` restore a file inside one
//! transaction, so a partial write is never committed. `fs.readbin` reads it
//! back for byte-for-byte verification. Input uses the same chunking.
//!
//! AIDEV-NOTE: this lives in Beckon, not in a throwaway probe, because the
//! keymap is a durable config file that can drift between the device and
//! Input's saved copy. The transport must be as durable as the file.

use anyhow::{Context, Result, anyhow, bail};

use crate::creator_micro::Pad;

const FS_CHUNK: usize = 3072; // 3072 raw bytes -> exactly 4096 base64 chars
const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[((n >> 18) & 63) as usize] as char);
        out.push(B64[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn b64_decode(input: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for c in input.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => continue,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

/// Write `bytes` to `remote` on the device filesystem in one transaction.
pub fn put(pad: &mut Pad, remote: &str, bytes: &[u8]) -> Result<()> {
    let begin = pad.call_json("fs.txbegin", serde_json::Value::Null)?;
    let tx = begin
        .get("tx")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| anyhow!("fs.txbegin returned no tx: {begin}"))?;
    let mut offset = 0usize;
    while offset < bytes.len() {
        let end = (offset + FS_CHUNK).min(bytes.len());
        pad.call_json(
            "fs.writebin",
            serde_json::json!({
                "file": remote,
                "data": b64_encode(&bytes[offset..end]),
                "append": true,
                "completed": end == bytes.len(),
                "offset": offset,
                "tx": tx,
            }),
        )
        .with_context(|| format!("fs.writebin at offset {offset}"))?;
        offset = end;
    }
    pad.call_json("fs.txcommit", serde_json::json!({ "tx": tx }))?;
    Ok(())
}

/// Read a file back off the device for verification.
pub fn get(pad: &mut Pad, remote: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    loop {
        let res = pad.call_json(
            "fs.readbin",
            serde_json::json!({ "file": remote, "offset": offset, "len": FS_CHUNK }),
        )?;
        let total = res
            .get("total_size")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(-1);
        let data = res
            .get("data")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let bytes = b64_decode(data)?;
        if bytes.is_empty() {
            break;
        }
        out.extend_from_slice(&bytes);
        offset += bytes.len();
        if total >= 0 && offset as i64 >= total {
            break;
        }
    }
    Ok(out)
}

/// Write only when the device copy differs, then verify the readback is
/// byte-identical. Never leaves a partially committed file.
pub fn ensure(pad: &mut Pad, remote: &str, bytes: &[u8]) -> Result<bool> {
    if get(pad, remote)? == bytes {
        return Ok(false);
    }
    put(pad, remote, bytes)?;
    let readback = get(pad, remote)?;
    if readback != bytes {
        bail!("{remote} readback differs after commit; device state is unverified");
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_binary_data() {
        let data: Vec<u8> = (0..=255u8).collect();
        assert_eq!(b64_decode(&b64_encode(&data)).unwrap(), data);
    }

    #[test]
    fn base64_handles_all_tail_lengths() {
        for len in 0..8 {
            let data = vec![0xABu8; len];
            assert_eq!(b64_decode(&b64_encode(&data)).unwrap(), data);
        }
    }

    #[test]
    fn base64_uses_standard_alphabet_and_padding() {
        assert_eq!(b64_encode(b"f"), "Zg==");
        assert_eq!(b64_encode(b"fo"), "Zm8=");
        assert_eq!(b64_encode(b"foo"), "Zm9v");
    }
}
