//! Minimal multi-entry ZIP reader for the Pocketful master contract (one
//! archive with `NSECompactScrip.csv`, `BSECompactScrip.csv`, ...). Entries
//! are located through the central directory, so archives written with
//! trailing data descriptors read correctly. Stored and deflated entries
//! only; ZIP64 is refused (each CSV is a few MB). The total inflated size is
//! capped (zip-bomb guard).

use crate::error::{AppError, Result};
use std::io::Read;

/// Refuse an archive that would inflate past this in total.
pub const MAX_UNCOMPRESSED: u64 = 768 * 1024 * 1024;

fn u16le(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

fn u32le(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn bad() -> AppError {
    AppError::Broker(
        "Pocketful's instrument file could not be read. Try downloading the master contract again."
            .into(),
    )
}

/// Every file entry of the archive, `(name, decompressed bytes)`, in
/// central-directory order.
pub fn entries(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let min = data.len().saturating_sub(22 + 65535);
    let eocd = (min..data.len().saturating_sub(21))
        .rev()
        .find(|&o| u32le(data, o) == Some(0x0605_4b50))
        .ok_or_else(bad)?;
    let count = u16le(data, eocd + 10).ok_or_else(bad)?;
    let mut cd = u32le(data, eocd + 16).ok_or_else(bad)? as usize;
    let mut total: u64 = 0;
    let mut out = Vec::new();
    for _ in 0..count {
        if u32le(data, cd) != Some(0x0201_4b50) {
            return Err(bad());
        }
        let method = u16le(data, cd + 10).ok_or_else(bad)?;
        let csize = u32le(data, cd + 20).ok_or_else(bad)? as usize;
        let usize_ = u64::from(u32le(data, cd + 24).ok_or_else(bad)?);
        let nlen = u16le(data, cd + 28).ok_or_else(bad)? as usize;
        let elen = u16le(data, cd + 30).ok_or_else(bad)? as usize;
        let clen = u16le(data, cd + 32).ok_or_else(bad)? as usize;
        let local = u32le(data, cd + 42).ok_or_else(bad)? as usize;
        let name = data.get(cd + 46..cd + 46 + nlen).ok_or_else(bad)?;
        let name = String::from_utf8_lossy(name).into_owned();
        cd += 46 + nlen + elen + clen;
        if name.ends_with('/') {
            continue;
        }
        if csize == u32::MAX as usize || usize_ == u64::from(u32::MAX) {
            return Err(bad());
        }
        total += usize_;
        if total > MAX_UNCOMPRESSED {
            return Err(bad());
        }
        if u32le(data, local) != Some(0x0403_4b50) {
            return Err(bad());
        }
        let lnlen = u16le(data, local + 26).ok_or_else(bad)? as usize;
        let lelen = u16le(data, local + 28).ok_or_else(bad)? as usize;
        let start = local + 30 + lnlen + lelen;
        let body = data.get(start..start + csize).ok_or_else(bad)?;
        let bytes = match method {
            0 => body.to_vec(),
            8 => {
                let mut buf = Vec::with_capacity(usize_ as usize);
                flate2::read::DeflateDecoder::new(body)
                    .take(usize_ + 1)
                    .read_to_end(&mut buf)
                    .map_err(|_| bad())?;
                if buf.len() as u64 > usize_ {
                    return Err(bad());
                }
                buf
            }
            _ => return Err(bad()),
        };
        out.push((name, bytes));
    }
    Ok(out)
}

/// Build an archive from `(name, content)` entries, deflated (tests and
/// fixtures).
#[cfg(any(test, feature = "test-support"))]
pub fn build(files: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    let mut out = Vec::new();
    let mut central = Vec::new();
    let push16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
    let push32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    for (name, content) in files {
        let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        let _ = e.write_all(content);
        let payload = e.finish().unwrap_or_default();
        let crc = crc32(content);
        let offset = out.len() as u32;
        push32(&mut out, 0x0403_4b50);
        push16(&mut out, 20);
        push16(&mut out, 0);
        push16(&mut out, 8);
        push32(&mut out, 0);
        push32(&mut out, crc);
        push32(&mut out, payload.len() as u32);
        push32(&mut out, content.len() as u32);
        push16(&mut out, name.len() as u16);
        push16(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&payload);
        push32(&mut central, 0x0201_4b50);
        push16(&mut central, 20);
        push16(&mut central, 20);
        push16(&mut central, 0);
        push16(&mut central, 8);
        push32(&mut central, 0);
        push32(&mut central, crc);
        push32(&mut central, payload.len() as u32);
        push32(&mut central, content.len() as u32);
        push16(&mut central, name.len() as u16);
        push16(&mut central, 0);
        push16(&mut central, 0);
        push16(&mut central, 0);
        push16(&mut central, 0);
        push32(&mut central, 0);
        push32(&mut central, offset);
        central.extend_from_slice(name.as_bytes());
    }
    let cd_start = out.len();
    out.extend_from_slice(&central);
    push32(&mut out, 0x0605_4b50);
    push16(&mut out, 0);
    push16(&mut out, 0);
    push16(&mut out, files.len() as u16);
    push16(&mut out, files.len() as u16);
    push32(&mut out, central.len() as u32);
    push32(&mut out, cd_start as u32);
    push16(&mut out, 0);
    out
}

#[cfg(any(test, feature = "test-support"))]
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_entry() {
        let a = b"exchange,token\nNSE,1\n".repeat(40);
        let b = b"exchange,token\nBSE,2\n".to_vec();
        let z = build(&[("NSECompactScrip.csv", &a), ("BSECompactScrip.csv", &b)]);
        let e = entries(&z).unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].0, "NSECompactScrip.csv");
        assert_eq!(e[0].1, a);
        assert_eq!(e[1].1, b);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn garbage_is_refused() {
        assert!(entries(b"not a zip").is_err());
        assert!(entries(&[]).is_err());
        let mut z = build(&[("a.csv", b"hello")]);
        let n = z.len();
        z.truncate(n - 30);
        assert!(entries(&z).is_err());
    }
}
