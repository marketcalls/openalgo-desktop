//! The SFeed `native_batch` decoder (web `streaming/sfeed_protocol.py`):
//! pure functions from a binary frame to packets.
//!
//! Binary frames are little-endian, several packets per frame, each
//! starting with its `u16` length. Header (9 bytes): `u16 length @0, u16 code
//! @2, i8 exchange @4, u8 level @5, u8 auction @6, u8 seq @7, u8 bitmask
//! length @8`. Codes 6511/6521 market open/close, 7207 index, 105 market
//! status, 104 closing-auction; otherwise level 1 is a mini touch line and
//! levels 2/4/8/16 a market picture (8 and 16 with depth rows). Prices are
//! integers divided by the exchange's divider (default 100); percentages
//! are /100. Body layouts (offsets from byte 9):
//! * index (78 bytes): `u32 token @0, i32 open @4, close @8, high @12,
//!   low @16, value @20, u64 ltt @24, i32 yearly high @32, yearly low @36,
//!   change % @40, f64 market cap @44, u8 precision @52, i32 multiplier
//!   @53, 21-byte name @57`
//! * mini (45 bytes): `u32 token @0, i64 ltt @4, u32 ltp @12, i64 ltq @16,
//!   u32 close @24, i32 change % @28, i32 change @32, u32 lot @36,
//!   u8 precision @40, u32 multiplier @41`
//! * market picture (135 bytes): `u32 token @0, i64 total buy @4, i64 total
//!   sell @12, i64 volume @20, i64 ltt @28, i64 last update @36, u32 open
//!   @44, close @48, high @52, low @56, ltp @60, i64 ltq @64, u32 atp @72,
//!   u32 indicative close @76, u32 buy rows @80, u32 sell rows @84, i16
//!   status @88, i32 change % @90, u32 OI @94, f64 turnover @98, i32 change
//!   @106, u32 upper @110, lower @114, yearly high @118, yearly low @122,
//!   lot @126, u8 precision @130, u32 multiplier @131`; then depth rows of
//!   `i64 quantity, i32 price, i32 orders` (a touch line always 1 + 1).

use serde_json::Value;
use std::collections::HashMap;

pub const HEADER_SIZE: usize = 9;
pub const DEFAULT_DIVIDER: f64 = 100.0;
pub const MSG_MARKET_OPEN: u16 = 6511;
pub const MSG_MARKET_CLOSE: u16 = 6521;
pub const MSG_INDEX: u16 = 7207;
pub const MSG_MARKET_STATUS: u16 = 105;
pub const MSG_CAS_CHANGE: u16 = 104;
const MP_FIXED_END: usize = 144;

/// SFeed exchange id -> segment name.
pub fn exchange_name(id: i8) -> Option<&'static str> {
    Some(match id {
        0 => "none",
        1 => "nse_cm",
        2 => "nse_fo",
        3 => "cde_fo",
        4 => "nse_com",
        5 => "bse_cm",
        6 => "bse_fo",
        7 => "bse_cd",
        8 => "bse_co",
        9 => "mcx_fo",
        10 => "ncd_co",
        _ => return None,
    })
}

fn exchange_id(name: &str) -> Option<i8> {
    (0..=10).find(|i| exchange_name(*i) == Some(name))
}

/// One depth row.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SfeedRow {
    pub quantity: i64,
    pub price: f64,
    pub orders: i64,
}

/// A decoded SFeed packet (web `decode_packet`).
#[derive(Debug, Clone, PartialEq)]
pub enum Sfeed {
    /// Market picture (levels 2, 4, 8, 16).
    Scrip {
        exchange: String,
        token: String,
        level: u8,
        ltp: f64,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        average_price: f64,
        last_trade_time: i64,
        last_trade_qty: i64,
        total_buy: i64,
        total_sell: i64,
        volume: i64,
        oi: i64,
        buy: Vec<SfeedRow>,
        sell: Vec<SfeedRow>,
    },
    /// Mini touch line (level 1): traded price and previous close only.
    Lite {
        exchange: String,
        token: String,
        ltp: f64,
        close: f64,
        last_trade_time: i64,
        last_trade_qty: i64,
    },
    Index {
        exchange: String,
        token: String,
        name: String,
        value: f64,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        last_trade_time: i64,
    },
    MarketStatus {
        exchange: String,
        code: u16,
    },
    Cas {
        exchange: String,
        token: String,
        ref_price: f64,
    },
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn i32le(b: &[u8], o: usize) -> i32 {
    i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn i64le(b: &[u8], o: usize) -> i64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    i64::from_le_bytes(a)
}

/// Split one binary frame into packets (web `split_batch`): a truncated or
/// garbage tail ends the scan without costing the packets before it.
pub fn split_batch(frame: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 2 <= frame.len() {
        let size = u16le(frame, off) as usize;
        if size < HEADER_SIZE || off + size > frame.len() {
            break;
        }
        out.push(&frame[off..off + size]);
        off += size;
    }
    out
}

fn decode_string(raw: &[u8]) -> String {
    let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).trim().to_string()
}

/// A scrip packet (levels 2, 4, 8, 16): the trade fields and the depth
/// rows the packet carries (one a side at level 4), within its length.
fn scrip_packet(
    p: &[u8],
    exchange: String,
    level: u8,
    div: f64,
    message_length: usize,
) -> Option<Sfeed> {
    if p.len() < MP_FIXED_END {
        return None;
    }
    let b = HEADER_SIZE;
    let price = |o: usize| f64::from(u32le(p, b + o)) / div;
    let (buy_n, sell_n) = if level == 4 {
        (1usize, 1usize)
    } else {
        (u32le(p, b + 80) as usize, u32le(p, b + 84) as usize)
    };
    let limit = message_length.min(p.len());
    let mut buy = Vec::new();
    let mut sell = Vec::new();
    let mut off = MP_FIXED_END;
    for i in 0..buy_n.saturating_add(sell_n) {
        if off + 16 > limit {
            break;
        }
        let row = SfeedRow {
            quantity: i64le(p, off),
            price: f64::from(i32le(p, off + 8)) / div,
            orders: i64::from(i32le(p, off + 12)),
        };
        if i < buy_n {
            buy.push(row);
        } else {
            sell.push(row);
        }
        off += 16;
    }
    Some(Sfeed::Scrip {
        exchange,
        token: u32le(p, b).to_string(),
        level,
        total_buy: i64le(p, b + 4),
        total_sell: i64le(p, b + 12),
        volume: i64le(p, b + 20),
        last_trade_time: i64le(p, b + 28),
        open: price(44),
        close: price(48),
        high: price(52),
        low: price(56),
        ltp: price(60),
        last_trade_qty: i64le(p, b + 64),
        average_price: price(72),
        oi: i64::from(u32le(p, b + 94)),
        buy,
        sell,
    })
}

/// Decode one packet with the auth response's dividers (exchange id ->
/// divider).
pub fn decode_packet(p: &[u8], dividers: &HashMap<i8, f64>) -> Option<Sfeed> {
    if p.len() < HEADER_SIZE {
        return None;
    }
    let message_length = u16le(p, 0) as usize;
    let code = u16le(p, 2);
    let ex_id = p[4] as i8;
    let level = p[5];
    let exchange = exchange_name(ex_id)
        .map(str::to_string)
        .unwrap_or_else(|| ex_id.to_string());
    let div = dividers
        .get(&ex_id)
        .copied()
        .filter(|d| *d != 0.0)
        .unwrap_or(DEFAULT_DIVIDER);
    let b = HEADER_SIZE;
    match code {
        MSG_MARKET_OPEN | MSG_MARKET_CLOSE => {
            return Some(Sfeed::MarketStatus {
                exchange,
                code: if code == MSG_MARKET_OPEN { 1 } else { 2 },
            })
        }
        MSG_INDEX => {
            if p.len() < b + 78 {
                return None;
            }
            let close = i32le(p, b + 8);
            return Some(Sfeed::Index {
                exchange,
                token: u32le(p, b).to_string(),
                name: decode_string(&p[b + 57..b + 78]),
                value: f64::from(i32le(p, b + 20)) / div,
                open: f64::from(i32le(p, b + 4)) / div,
                high: f64::from(i32le(p, b + 12)) / div,
                low: f64::from(i32le(p, b + 16)) / div,
                close: f64::from(close) / div,
                last_trade_time: i64le(p, b + 24),
            });
        }
        MSG_MARKET_STATUS => {
            if p.len() < b + 7 {
                return None;
            }
            return Some(Sfeed::MarketStatus {
                exchange,
                code: u16le(p, b),
            });
        }
        MSG_CAS_CHANGE => {
            if p.len() < b + 24 {
                return None;
            }
            let (r, q1, q2) = (u32le(p, b + 4), i64le(p, b + 8), i64le(p, b + 16));
            // All-zero outside the closing auction: nothing to say.
            if r == 0 && q1 == 0 && q2 == 0 {
                return None;
            }
            return Some(Sfeed::Cas {
                exchange,
                token: u32le(p, b).to_string(),
                ref_price: f64::from(r) / div,
            });
        }
        _ => {}
    }
    match level {
        1 => {
            if p.len() < b + 45 {
                return None;
            }
            Some(Sfeed::Lite {
                exchange,
                token: u32le(p, b).to_string(),
                last_trade_time: i64le(p, b + 4),
                ltp: f64::from(u32le(p, b + 12)) / div,
                last_trade_qty: i64le(p, b + 16),
                close: f64::from(u32le(p, b + 24)) / div,
            })
        }
        2 | 4 | 8 | 16 => scrip_packet(p, exchange, level, div, message_length),
        _ => None,
    }
}

/// Dividers from the auth response: `exchanges: {name: {value, divider}}`
/// keyed by the stated id (falling back to the static name map).
pub fn parse_dividers(v: &Value) -> HashMap<i8, f64> {
    let mut out = HashMap::new();
    if let Some(ex) = v.get("exchanges").and_then(Value::as_object) {
        for (name, info) in ex {
            let Some(info) = info.as_object() else {
                continue;
            };
            let id = info
                .get("value")
                .and_then(Value::as_i64)
                .and_then(|i| i8::try_from(i).ok())
                .or_else(|| exchange_id(name));
            if let Some(id) = id {
                let d = info.get("divider").and_then(Value::as_f64).unwrap_or(100.0);
                out.insert(id, d);
            }
        }
    }
    out
}
