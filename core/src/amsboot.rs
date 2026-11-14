//! Port of Rockbox mkamsboot for the Sansa Clip+. It patches the original
//! firmware (OF) so the player boots Rockbox, and boots the OF when Left or
//! Home is held at power on.

use crate::net;
use md5::{Digest, Md5};
use std::ffi::{c_int, c_uint, c_void};
use std::ptr::{null, null_mut};
use std::sync::OnceLock;

const OF_MD5: [(&str, &str); 5] = [
    ("01.02.09", "656d38114774c2001dc18e6726df3c5d"),
    ("01.02.13", "5f89872b79ef440b0e5ee3a7a44328b2"),
    ("01.02.15", "680a4f521e790ad25b93b1b16f3a207d"),
    ("01.02.16", "055a53de1dfb09f6cb71c504ad48bd13"),
    ("01.02.18", "80b547244438b113e2a55ff0305f12c0"),
];

const RB_MODEL_NAME: &[u8; 4] = b"cli+";
const RB_MODEL_NUM: u32 = 66;
// IRAM the OF boots with, mapped at 0x0.
const RAM_SIZE: usize = 320 << 10;
// Where the OF firmware block starts, and where the dual boot stub goes.
const BLOCK: usize = 0x400;
const STUB: usize = 0x600;
const FW_SIZE_AT: usize = 0x10;
const UCL_LEVEL: c_int = 10;
const UCL_OK: c_int = 0;

unsafe extern "C" {
    fn clipplus_ucl_init() -> c_int;
    fn ucl_nrv2e_99_compress(
        src: *const u8,
        src_len: c_uint,
        dst: *mut u8,
        dst_len: *mut c_uint,
        cb: *const c_void,
        level: c_int,
        conf: *const c_void,
        result: *mut c_uint,
    ) -> c_int;
    fn ucl_nrv2e_decompress_safe_8(
        src: *const u8,
        src_len: c_uint,
        dst: *mut u8,
        dst_len: *mut c_uint,
        wrkmem: *mut c_void,
    ) -> c_int;
}

pub fn md5_hex(data: &[u8]) -> String {
    net::hex(&Md5::digest(data))
}

/// The stock firmware version for a known Clip+ `clppa.bin`.
pub fn of_version(of: &[u8]) -> Option<&'static str> {
    let md5 = md5_hex(of);
    OF_MD5.iter().find(|(_, m)| *m == md5).map(|(v, _)| *v)
}

fn le32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(buf[at..at + 4].try_into().unwrap())
}

fn put32(buf: &mut [u8], at: usize, value: u32) {
    buf[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn sum32(buf: &[u8]) -> u32 {
    let (words, _) = buf.as_chunks::<4>();
    words
        .iter()
        .fold(0, |sum, word| sum.wrapping_add(u32::from_le_bytes(*word)))
}

fn ucl_ready() -> Result<(), String> {
    static READY: OnceLock<bool> = OnceLock::new();
    // SAFETY: the shim only checks type sizes.
    if *READY.get_or_init(|| unsafe { clipplus_ucl_init() } == UCL_OK) {
        Ok(())
    } else {
        Err("UCL library self check failed".into())
    }
}

fn compress(data: &[u8]) -> Result<Vec<u8>, String> {
    ucl_ready()?;
    let len = c_uint::try_from(data.len()).map_err(|e| e.to_string())?;
    let cap = data.len() + data.len() / 8 + 256;
    let mut out = vec![0u8; cap];
    let mut out_len: c_uint = 0;
    // SAFETY: out holds the worst case size from the UCL documentation.
    let rc = unsafe {
        ucl_nrv2e_99_compress(
            data.as_ptr(),
            len,
            out.as_mut_ptr(),
            &mut out_len,
            null(),
            UCL_LEVEL,
            null(),
            null_mut(),
        )
    };
    if rc != UCL_OK || out_len as usize > cap {
        return Err(format!("compression failed ({rc})"));
    }
    out.truncate(out_len as usize);
    Ok(out)
}

fn decompress(packed: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    let src_len = c_uint::try_from(packed.len()).map_err(|e| e.to_string())?;
    let mut out = vec![0u8; expected];
    let mut out_len = c_uint::try_from(expected).map_err(|e| e.to_string())?;
    // SAFETY: the safe decoder stops at out_len bytes of output.
    let rc = unsafe {
        ucl_nrv2e_decompress_safe_8(
            packed.as_ptr(),
            src_len,
            out.as_mut_ptr(),
            &mut out_len,
            null_mut(),
        )
    };
    if rc != UCL_OK || out_len as usize != expected {
        return Err(format!("decompression check failed ({rc})"));
    }
    Ok(out)
}

/// The Rockbox bootloader payload after its 8 byte header.
fn rockbox_payload(file: &[u8]) -> Result<&[u8], String> {
    let (header, payload) = file
        .split_at_checked(8)
        .ok_or("bootloader file is too short")?;
    if &header[4..8] != RB_MODEL_NAME {
        return Err("bootloader file is not for the Clip+".into());
    }
    let sum = payload
        .iter()
        .fold(RB_MODEL_NUM, |sum, &b| sum.wrapping_add(u32::from(b)));
    if sum != u32::from_be_bytes(header[..4].try_into().unwrap()) {
        return Err("bootloader checksum mismatch".into());
    }
    Ok(payload)
}

/// Checks the OF is a known Clip+ file and returns its firmware block size.
fn check_of(of: &[u8]) -> Result<usize, String> {
    if of_version(of).is_none() {
        return Err("original firmware version is not supported".into());
    }
    let (body, last) = of
        .split_at_checked(of.len().saturating_sub(4))
        .filter(|(body, last)| body.len().is_multiple_of(4) && last.len() == 4)
        .ok_or("original firmware has a bad size")?;
    // Stored as the bytes de ad be ef when a file carries no checksum.
    let last = u32::from_le_bytes(last.try_into().unwrap());
    if last != 0xefbe_adde && last != sum32(body) {
        return Err("original firmware checksum failed".into());
    }
    Ok(le32(of, FW_SIZE_AT) as usize)
}

fn check_sizes(
    rb_packed: usize,
    rb_unpacked: usize,
    of_packed: usize,
    of_unpacked: usize,
) -> Result<(), String> {
    let packed = DUALBOOT_CLIPPLUS.len() + NRV2E_D8.len() + of_packed + rb_packed + 0x200;
    let ram_rb = NRV2E_D8.len() + rb_packed + rb_unpacked;
    let ram_of = NRV2E_D8.len() + of_packed + of_unpacked;
    if packed > of_unpacked {
        return Err(format!(
            "packed data ({packed} bytes) does not fit in the firmware ({of_unpacked} bytes)"
        ));
    }
    if ram_rb > RAM_SIZE || ram_of > RAM_SIZE {
        return Err("firmware does not fit in player memory when unpacked".into());
    }
    Ok(())
}

/// Returns a patched copy of the OF that boots Rockbox. The result has passed
/// every check, or this returns an error.
pub fn patch(of: &[u8], bootloader: &[u8]) -> Result<Vec<u8>, String> {
    let payload = rockbox_payload(bootloader)?;
    let fw_size = check_of(of)?;
    build(of, payload, fw_size)
}

fn build(of: &[u8], payload: &[u8], fw_size: usize) -> Result<Vec<u8>, String> {
    let end = BLOCK + fw_size;
    if !fw_size.is_multiple_of(4) || of.len() < STUB + fw_size {
        return Err("firmware block size is not valid".into());
    }
    let of_block = &of[BLOCK..end];
    let of_packed = compress(of_block)?;
    let rb_packed = compress(payload)?;
    if decompress(&of_packed, of_block.len())? != of_block
        || decompress(&rb_packed, payload.len())? != payload
    {
        return Err("compressed data does not match its input".into());
    }
    check_sizes(rb_packed.len(), payload.len(), of_packed.len(), fw_size)?;

    let mut buf = of.to_vec();
    buf[STUB..STUB + fw_size].fill(0);
    buf[STUB..STUB + DUALBOOT_CLIPPLUS.len()].copy_from_slice(&DUALBOOT_CLIPPLUS);

    const GOTO_START: u32 = 0xe3a0_fc02; // mov pc, #0x200
    const LOOP: u32 = 0xeaff_fffe; // b .
    for i in 0..8 {
        put32(&mut buf, BLOCK + 4 * i, LOOP);
    }
    put32(&mut buf, BLOCK, GOTO_START);
    put32(&mut buf, BLOCK + 8, GOTO_START);

    // Filled backwards from the end: decoder, packed OF, packed Rockbox.
    let decoder_at = end - NRV2E_D8.len();
    let of_at = decoder_at - of_packed.len();
    let rb_at = of_at - rb_packed.len();
    buf[decoder_at..end].copy_from_slice(&NRV2E_D8);
    buf[of_at..decoder_at].copy_from_slice(&of_packed);
    buf[rb_at..of_at].copy_from_slice(&rb_packed);

    // Last byte and size of each image, read by the stub.
    let word = |n: usize| u32::try_from(n).map_err(|e| e.to_string());
    put32(&mut buf, 0x604, word(fw_size - 1)?);
    put32(&mut buf, 0x608, word(NRV2E_D8.len())?);
    put32(&mut buf, 0x60c, word(fw_size - NRV2E_D8.len() - 1)?);
    put32(&mut buf, 0x610, word(of_packed.len())?);
    put32(
        &mut buf,
        0x614,
        word(fw_size - NRV2E_D8.len() - of_packed.len() - 1)?,
    );
    put32(&mut buf, 0x618, word(rb_packed.len())?);
    put32(&mut buf, 0x61c, word(RAM_SIZE - 1)?);

    let block_sum = sum32(&buf[BLOCK..end]);
    put32(&mut buf, 0x08, block_sum);
    put32(&mut buf, 0x208, block_sum);
    let header_sum = sum32(&buf[..0x1fc]);
    put32(&mut buf, 0x1fc, header_sum);
    let header_sum = sum32(&buf[0x200..0x3fc]);
    put32(&mut buf, 0x3fc, header_sum);
    let file_sum = sum32(&buf[..buf.len() - 4]);
    let len = buf.len();
    put32(&mut buf, len - 4, file_sum);

    verify(&buf, of_block, payload, fw_size)?;
    Ok(buf)
}

/// Reads the patched file back the way the player does.
fn verify(buf: &[u8], of_block: &[u8], payload: &[u8], fw_size: usize) -> Result<(), String> {
    let end = BLOCK + fw_size;
    let decoder_at = end - NRV2E_D8.len();
    let of_len = le32(buf, 0x610) as usize;
    let rb_len = le32(buf, 0x618) as usize;
    let of_at = decoder_at - of_len;
    let rb_at = of_at - rb_len;
    let block_sum = sum32(&buf[BLOCK..end]);
    let ok = buf[decoder_at..end] == NRV2E_D8
        && decompress(&buf[of_at..decoder_at], of_block.len())? == of_block
        && decompress(&buf[rb_at..of_at], payload.len())? == payload
        && le32(buf, 0x08) == block_sum
        && le32(buf, 0x208) == block_sum
        && le32(buf, 0x1fc) == sum32(&buf[..0x1fc])
        && le32(buf, 0x3fc) == sum32(&buf[0x200..0x3fc])
        && le32(buf, buf.len() - 4) == sum32(&buf[..buf.len() - 4]);
    if ok {
        Ok(())
    } else {
        Err("patched firmware failed verification".into())
    }
}

// From utils/mkamsboot/dualboot.c in the Rockbox source (generated from
// dualboot.S and nrv2e_d8.S, GPL-2.0-or-later).
const DUALBOOT_CLIPPLUS: [u8; 184] = [
    0x06, 0x00, 0x00, 0xea, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x24, 0x00, 0x1f, 0xe5, 0x24, 0x10, 0x1f, 0xe5, 0x01, 0x20, 0x40, 0xe0, 0x18, 0x30, 0x1f, 0xe5,
    0x01, 0x40, 0x50, 0xe4, 0x01, 0x40, 0x43, 0xe4, 0x00, 0x00, 0x52, 0xe1, 0xfb, 0xff, 0xff, 0x1a,
    0x2c, 0x30, 0x0f, 0xe5, 0x60, 0x00, 0x9f, 0xe5, 0x00, 0x10, 0x90, 0xe5, 0x01, 0x18, 0x81, 0xe3,
    0x00, 0x10, 0x80, 0xe5, 0x54, 0x00, 0x9f, 0xe5, 0x20, 0x10, 0x90, 0xe5, 0x50, 0x00, 0x9f, 0xe5,
    0x08, 0x20, 0x90, 0xe5, 0x01, 0x20, 0x82, 0xe1, 0x00, 0x00, 0x52, 0xe3, 0x02, 0x00, 0x00, 0x1a,
    0x64, 0x00, 0x1f, 0xe5, 0x64, 0x10, 0x1f, 0xe5, 0x01, 0x00, 0x00, 0xea, 0x78, 0x00, 0x1f, 0xe5,
    0x78, 0x10, 0x1f, 0xe5, 0x70, 0x30, 0x1f, 0xe5, 0x02, 0x50, 0x83, 0xe2, 0x01, 0x40, 0x43, 0xe0,
    0x01, 0x20, 0x50, 0xe4, 0x01, 0x20, 0x43, 0xe4, 0x04, 0x00, 0x53, 0xe1, 0xfb, 0xff, 0xff, 0x1a,
    0x01, 0x00, 0x83, 0xe2, 0x00, 0x20, 0xa0, 0xe3, 0x15, 0xff, 0x2f, 0xe1, 0x14, 0x00, 0x0f, 0xc8,
    0x00, 0x00, 0x0d, 0xc8, 0x00, 0x00, 0x0b, 0xc8,
];

const NRV2E_D8: [u8; 168] = [
    0x0f, 0x18, 0x01, 0x24, 0x65, 0x42, 0xe4, 0x07, 0x05, 0x26, 0x36, 0x02, 0x0a, 0xe0, 0x00, 0x20,
    0x00, 0x47, 0x04, 0x78, 0x64, 0x41, 0x01, 0x30, 0x24, 0x06, 0xf7, 0x46, 0x03, 0x78, 0x01, 0x30,
    0x13, 0x70, 0x01, 0x32, 0x24, 0x19, 0xfe, 0x46, 0xf3, 0xd0, 0xf7, 0xd2, 0x01, 0x21, 0x04, 0xe0,
    0x01, 0x39, 0x24, 0x19, 0xfe, 0x46, 0xec, 0xd0, 0x49, 0x41, 0x24, 0x19, 0xfe, 0x46, 0xe8, 0xd0,
    0x49, 0x41, 0x24, 0x19, 0xfe, 0x46, 0xe4, 0xd0, 0xf2, 0xd3, 0xcb, 0x1e, 0x00, 0x21, 0x08, 0xd3,
    0x1b, 0x02, 0x05, 0x78, 0x01, 0x30, 0x1d, 0x43, 0xed, 0x43, 0xd8, 0xd0, 0x6d, 0x10, 0x13, 0xd2,
    0x03, 0xe0, 0x24, 0x19, 0xfe, 0x46, 0xd4, 0xd0, 0x0e, 0xd2, 0x01, 0x21, 0x24, 0x19, 0xfe, 0x46,
    0xcf, 0xd0, 0x09, 0xd2, 0x24, 0x19, 0xfe, 0x46, 0xcb, 0xd0, 0x49, 0x41, 0x24, 0x19, 0xfe, 0x46,
    0xc7, 0xd0, 0xf7, 0xd3, 0x04, 0x31, 0x04, 0xe0, 0x24, 0x19, 0xfe, 0x46, 0xc1, 0xd0, 0x49, 0x41,
    0x02, 0x31, 0xee, 0x42, 0x00, 0xd2, 0x01, 0x31, 0x13, 0x78, 0x53, 0x5d, 0x13, 0x70, 0x01, 0x32,
    0x01, 0x39, 0xfa, 0xd1, 0xbe, 0xe7, 0x00, 0x00,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn bootloader_file(model: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let sum = payload
            .iter()
            .fold(RB_MODEL_NUM, |s, &b| s.wrapping_add(u32::from(b)));
        let mut file = sum.to_be_bytes().to_vec();
        file.extend_from_slice(model);
        file.extend_from_slice(payload);
        file
    }

    fn synthetic_of(fw_size: usize) -> Vec<u8> {
        let mut of = vec![0u8; STUB + fw_size + 0x40];
        for (i, b) in of[BLOCK..BLOCK + fw_size].iter_mut().enumerate() {
            *b = if i % 5 == 0 { (i % 251) as u8 } else { 0 };
        }
        of[0x100..0x108].copy_from_slice(b"01.02.18");
        put32(&mut of, FW_SIZE_AT, fw_size as u32);
        of
    }

    #[test]
    fn hashes_md5() {
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn rockbox_header_is_checked() {
        let payload = b"payload bytes".as_slice();
        let good = bootloader_file(RB_MODEL_NAME, payload);
        assert_eq!(rockbox_payload(&good).unwrap(), payload);

        let wrong_model = bootloader_file(b"clip", payload);
        assert!(rockbox_payload(&wrong_model).unwrap_err().contains("Clip+"));

        let mut bad_sum = good.clone();
        *bad_sum.last_mut().unwrap() ^= 1;
        assert!(rockbox_payload(&bad_sum).unwrap_err().contains("checksum"));

        assert!(rockbox_payload(&good[..5]).is_err());
    }

    #[test]
    fn rejects_unknown_firmware() {
        let bootloader = bootloader_file(RB_MODEL_NAME, b"payload bytes");
        assert!(of_version(&synthetic_of(0x4000)).is_none());
        let err = patch(&synthetic_of(0x4000), &bootloader).unwrap_err();
        assert!(err.contains("not supported"));
    }

    #[test]
    fn check_sizes_fails_when_data_does_not_fit() {
        assert!(check_sizes(100, 200, 1000, 4000).is_ok());
        let err = check_sizes(100, 200, 4000, 4000).unwrap_err();
        assert!(err.contains("does not fit in the firmware"));
        let err = check_sizes(10, 10, 10, RAM_SIZE).unwrap_err();
        assert!(err.contains("player memory"));
    }

    #[test]
    fn build_lays_out_a_verified_image() {
        let fw_size = 0x4000;
        let of = synthetic_of(fw_size);
        let payload: Vec<u8> = (0..3000u32).map(|i| (i % 13) as u8).collect();
        let out = build(&of, &payload, fw_size).unwrap();
        assert_eq!(out.len(), of.len());
        assert_eq!(le32(&out, BLOCK), 0xe3a0_fc02);
        assert_eq!(le32(&out, BLOCK + 4), 0xeaff_fffe);
        assert_eq!(le32(&out, 0x61c) as usize, RAM_SIZE - 1);
        assert_eq!(out[0x100..0x110], of[0x100..0x110]);
    }

    #[test]
    fn build_fails_when_the_firmware_is_too_small() {
        let fw_size = 0x300;
        let payload: Vec<u8> = (0..3000u32).map(|i| (i * 7 % 253) as u8).collect();
        let err = build(&synthetic_of(fw_size), &payload, fw_size).unwrap_err();
        assert!(err.contains("does not fit"));
    }

    #[test]
    fn build_rejects_an_out_of_range_firmware_size() {
        let of = synthetic_of(0x4000);
        assert!(build(&of, b"x", 0x4000 + 0x1000).is_err());
        assert!(build(&of, b"x", 0x4002).is_err());
    }

    #[test]
    fn check_of_rejects_unknown_input() {
        assert!(check_of(&[]).is_err());
        assert!(check_of(&[0u8; 7]).is_err());
    }
}
