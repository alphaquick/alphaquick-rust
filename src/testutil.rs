//! Small synthetic files for the unit tests, written in the real container format.

#![allow(clippy::too_many_arguments)]

/// 1.0 in the format's fixed point.
pub const P: i64 = 100_000_000;
/// 2026-10-23 00:00:00 UTC in ticks.
pub const T0: i64 = 639_283_104_000_000_000;

fn scaled(value: f64) -> i64 {
    (value * P as f64).round() as i64
}

/// One K4os pickler frame holding `block`, compressed when that saves bytes.
pub fn frame(block: &[u8], compress: bool) -> Vec<u8> {
    if compress {
        let mut packed = vec![0u8; lz4_flex::block::get_maximum_output_size(block.len())];
        let n = lz4_flex::block::compress_into(block, &mut packed).unwrap();
        packed.truncate(n);
        if block.len() > n {
            let diff = block.len() - n;
            let (code, width) = if diff < 0x100 {
                (1u8, 1)
            } else if diff < 0x10000 {
                (2, 2)
            } else {
                (3, 4)
            };
            let mut out = vec![code << 6];
            out.extend_from_slice(&(diff as u32).to_le_bytes()[..width]);
            out.extend_from_slice(&packed);
            return out;
        }
    }
    let mut out = vec![0u8];
    out.extend_from_slice(block);
    out
}

/// A whole file: the size hint, then one frame per block.
pub fn container(blocks: &[Vec<u8>], compress: bool) -> Vec<u8> {
    let mut out = 65536i32.to_le_bytes().to_vec();
    for block in blocks {
        let f = frame(block, compress);
        out.extend_from_slice(&(f.len() as i32).to_le_bytes());
        out.extend_from_slice(&f);
    }
    out
}

pub fn header(kind: i16, size: u16, time: i64) -> Vec<u8> {
    let mut out = kind.to_le_bytes().to_vec();
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&time.to_le_bytes());
    out
}

// -- L2 ----------------------------------------------------------------------

pub fn depth(time: i64, price: f64, volume: f64, flags: u8) -> Vec<u8> {
    let mut out = header(0, 29, time);
    out.extend_from_slice(&scaled(price).to_le_bytes());
    out.extend_from_slice(&scaled(volume).to_le_bytes());
    out.push(flags);
    out
}

pub fn l2_trade(time: i64, price: f64, volume: f64, side: u8, trade_id: i64) -> Vec<u8> {
    let mut out = header(1, 37, time);
    out.extend_from_slice(&trade_id.to_le_bytes());
    out.extend_from_slice(&scaled(price).to_le_bytes());
    out.extend_from_slice(&scaled(volume).to_le_bytes());
    out.push(side);
    out
}

pub fn unknown(kind: i16, time: i64) -> Vec<u8> {
    let mut out = header(kind, 15, time);
    out.extend_from_slice(&[1, 2, 3]);
    out
}

// -- order log -----------------------------------------------------------------

pub fn meta(coin: &str, flags: u16) -> Vec<u8> {
    let mut body = b"HLC1".to_vec();
    body.extend_from_slice(&1u16.to_le_bytes());
    body.push(2); // size decimals
    body.push(0); // dex
    body.extend_from_slice(&7u16.to_le_bytes());
    body.extend_from_slice(&flags.to_le_bytes());
    body.extend_from_slice(&20261023u32.to_le_bytes());
    body.extend_from_slice(&1000u64.to_le_bytes());
    body.extend_from_slice(&1_792_713_600_000_000_000u64.to_le_bytes());
    body.push(coin.len() as u8);
    body.extend_from_slice(coin.as_bytes());
    let mut out = header(16, (12 + body.len()) as u16, T0);
    out.extend_from_slice(&body);
    out
}

pub fn block(number: u64, time: i64) -> Vec<u8> {
    let ns = ((time - 621_355_968_000_000_000) * 100) as u64;
    let mut out = header(17, 36, time);
    out.extend_from_slice(&number.to_le_bytes());
    out.extend_from_slice(&ns.to_le_bytes());
    out.extend_from_slice(&(ns + 300_000_000).to_le_bytes());
    out
}

pub fn user(user_id: u32, address_byte: u8, time: i64) -> Vec<u8> {
    let mut out = header(21, 36, time);
    out.extend_from_slice(&user_id.to_le_bytes());
    out.extend_from_slice(&[address_byte; 20]);
    out
}

pub fn snap(oid: u64, side: u8, price: f64, size: f64, user_id: u32, flags: u8) -> Vec<u8> {
    let mut out = header(20, 44, T0);
    out.extend_from_slice(&[side, 1, flags, 0]);
    out.extend_from_slice(&oid.to_le_bytes());
    out.extend_from_slice(&scaled(price).to_le_bytes());
    out.extend_from_slice(&scaled(size).to_le_bytes());
    out.extend_from_slice(&user_id.to_le_bytes());
    out
}

/// An ORDER record; `tif` 1 (Gtc), order type 0 (Limit) and flags 0 unless given by `order_with`.
pub fn order(
    status: u8,
    oid: u64,
    side: u8,
    price: f64,
    size: f64,
    user_id: u32,
    time: i64,
) -> Vec<u8> {
    order_with(status, oid, side, price, size, user_id, 1, 0, 0, time)
}

pub fn order_with(
    status: u8,
    oid: u64,
    side: u8,
    price: f64,
    size: f64,
    user_id: u32,
    tif: u8,
    order_type: u8,
    flags: u8,
    time: i64,
) -> Vec<u8> {
    let mut out = header(18, 45, time);
    out.extend_from_slice(&[status, side, tif, order_type, flags]);
    out.extend_from_slice(&oid.to_le_bytes());
    out.extend_from_slice(&scaled(price).to_le_bytes());
    out.extend_from_slice(&scaled(size).to_le_bytes());
    out.extend_from_slice(&user_id.to_le_bytes());
    out
}

pub fn trade(
    taker_side: u8,
    price: f64,
    size: f64,
    maker_oid: u64,
    taker_oid: u64,
    maker: u32,
    taker: u32,
    flags: u8,
    time: i64,
) -> Vec<u8> {
    let mut out = header(19, 70, time);
    out.extend_from_slice(&[taker_side, flags]);
    out.extend_from_slice(&scaled(price).to_le_bytes());
    out.extend_from_slice(&scaled(size).to_le_bytes());
    out.extend_from_slice(&maker_oid.to_le_bytes());
    out.extend_from_slice(&taker_oid.to_le_bytes());
    out.extend_from_slice(&maker.to_le_bytes());
    out.extend_from_slice(&taker.to_le_bytes());
    out.extend_from_slice(&scaled(1.5).to_le_bytes());
    out.extend_from_slice(&scaled(-2.0).to_le_bytes());
    out
}

pub fn outcome(text: &str) -> Vec<u8> {
    let mut body = 1u16.to_le_bytes().to_vec();
    body.extend_from_slice(text.as_bytes());
    let mut out = header(22, (12 + body.len()) as u16, T0);
    out.extend_from_slice(&body);
    out
}
