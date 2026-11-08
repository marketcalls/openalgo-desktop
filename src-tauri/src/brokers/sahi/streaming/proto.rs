//! Protobuf wire codec for the Sahi market-data stream (web
//! `streaming/sahi_proto.py`), package `sahi.direct.marketdata.v1`.
//!
//! One `ClientMessage` or `ServerMessage` per binary frame, no length
//! prefix. Written by hand, like the web, because the schema is small and
//! the decoder must accept packed and unpacked repeated ids and skip
//! unknown fields by wire type. Proto3 `optional` fields decode to `None`
//! when absent, so an instrument without open interest is not read as 0.

/// Sahi `Mode` enum.
pub(crate) const MODE_LTP: u64 = 1;
pub(crate) const MODE_QUOTE: u64 = 2;
pub(crate) const MODE_DEPTH: u64 = 3;

/// Sahi `ExchangeSegment` enum.
pub(crate) const SEGMENT_NSE: u64 = 1;
pub(crate) const SEGMENT_BSE: u64 = 2;
pub(crate) const SEGMENT_NFO: u64 = 3;
pub(crate) const SEGMENT_BFO: u64 = 4;

/// Sahi `AckAction` enum.
pub(crate) const ACK_SUBSCRIBE: u64 = 1;
pub(crate) const ACK_UNSUBSCRIBE: u64 = 2;
pub(crate) const ACK_MODE_CHANGE: u64 = 3;

const WT_VARINT: u64 = 0;
const WT_I64: u64 = 1;
const WT_LEN: u64 = 2;
const WT_I32: u64 = 5;

/// A frame that is not well-formed protobuf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodeError(pub(crate) &'static str);

type Decoded<T> = std::result::Result<T, DecodeError>;

/// Which tick message a frame carried; a deeper tick serves every lower
/// OpenAlgo mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum TickKind {
    Ltp = 1,
    Quote = 2,
    Depth = 3,
}

/// One depth level, prices in paise.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Level {
    pub price: i64,
    pub qty: i64,
    pub orders: i64,
}

/// `LtpTick` / `QuoteTick` / `DepthTick`: they share field numbers 1-12.
/// Only what OpenAlgo publishes is kept; `instrument_type` (3) and
/// `prev_open_interest` (9) are read past, since the web publishes
/// neither.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Tick {
    pub instrument_id: i64,
    pub exchange_segment: u64,
    pub last_trade_price: i64,
    pub last_trade_time: Option<i64>,
    pub volume_today: i64,
    /// The daily price band (`dpr_upper`, `dpr_lower`), in paise; 0 when
    /// the tick left it off (a proto3 scalar).
    pub dpr_upper: i64,
    pub dpr_lower: i64,
    pub open_interest: Option<i64>,
    pub best_buys: Vec<Level>,
    pub best_sells: Vec<Level>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Ack {
    pub action: u64,
    pub instrument_ids: Vec<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StreamError {
    pub code: String,
    pub message: String,
}

/// One decoded `ServerMessage` (its oneof).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ServerMessage {
    Ack(Ack),
    Tick(TickKind, Tick),
    Error(StreamError),
    /// The oneof was unset or a payload this codec does not know.
    Empty,
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

fn put_varint(mut v: u64, out: &mut Vec<u8>) {
    while v > 0x7F {
        out.push(((v & 0x7F) as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_key(field: u64, wire_type: u64, out: &mut Vec<u8>) {
    put_varint((field << 3) | wire_type, out);
}

fn put_len(field: u64, payload: &[u8], out: &mut Vec<u8>) {
    put_key(field, WT_LEN, out);
    put_varint(payload.len() as u64, out);
    out.extend_from_slice(payload);
}

/// Packed `repeated int64`; proto3 omits an empty one. Negative ids are
/// sent as 64-bit two's complement, as protobuf int64 requires.
fn put_packed_i64(field: u64, ids: &[i64], out: &mut Vec<u8>) {
    if ids.is_empty() {
        return;
    }
    let mut packed = Vec::with_capacity(ids.len() * 4);
    for id in ids {
        put_varint(*id as u64, &mut packed);
    }
    put_len(field, &packed, out);
}

fn mode_and_ids(mode: u64, ids: &[i64]) -> Vec<u8> {
    let mut body = Vec::new();
    if mode != 0 {
        put_key(1, WT_VARINT, &mut body);
        put_varint(mode, &mut body);
    }
    put_packed_i64(2, ids, &mut body);
    body
}

/// `ClientMessage{subscribe = Subscribe{mode, instrument_ids}}`.
pub(crate) fn encode_subscribe(mode: u64, ids: &[i64]) -> Vec<u8> {
    let mut out = Vec::new();
    put_len(1, &mode_and_ids(mode, ids), &mut out);
    out
}

/// `ClientMessage{unsubscribe = Unsubscribe{instrument_ids}}`.
pub(crate) fn encode_unsubscribe(ids: &[i64]) -> Vec<u8> {
    let mut body = Vec::new();
    put_packed_i64(1, ids, &mut body);
    let mut out = Vec::new();
    put_len(2, &body, &mut out);
    out
}

/// `ClientMessage{mode_change = ModeChange{mode, instrument_ids}}`.
pub(crate) fn encode_mode_change(mode: u64, ids: &[i64]) -> Vec<u8> {
    let mut out = Vec::new();
    put_len(3, &mode_and_ids(mode, ids), &mut out);
    out
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

/// A bounded reader over one message's bytes.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn done(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn varint(&mut self) -> Decoded<u64> {
        let mut result: u64 = 0;
        let mut shift = 0u32;
        loop {
            let b = *self
                .buf
                .get(self.pos)
                .ok_or(DecodeError("truncated varint"))?;
            self.pos += 1;
            if shift < 64 {
                result |= u64::from(b & 0x7F) << shift;
            }
            if b < 0x80 {
                return Ok(result);
            }
            shift += 7;
            if shift >= 70 {
                return Err(DecodeError("varint too long"));
            }
        }
    }

    fn key(&mut self) -> Decoded<(u64, u64)> {
        let k = self.varint()?;
        Ok((k >> 3, k & 7))
    }

    /// The payload of a length-delimited field (the key already read).
    fn bytes(&mut self) -> Decoded<&'a [u8]> {
        let len = usize::try_from(self.varint()?).map_err(|_| DecodeError("length too large"))?;
        let end = self
            .pos
            .checked_add(len)
            .filter(|e| *e <= self.buf.len())
            .ok_or(DecodeError("truncated length-delimited field"))?;
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn advance(&mut self, n: usize) -> Decoded<()> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.buf.len())
            .ok_or(DecodeError("truncated field"))?;
        self.pos = end;
        Ok(())
    }

    fn skip(&mut self, wire_type: u64) -> Decoded<()> {
        match wire_type {
            WT_VARINT => self.varint().map(|_| ()),
            WT_I64 => self.advance(8),
            WT_LEN => self.bytes().map(|_| ()),
            WT_I32 => self.advance(4),
            // Groups (3/4) are not proto3; anything else is corrupt.
            _ => Err(DecodeError("unsupported wire type")),
        }
    }

    /// `repeated int64`, packed or one unpacked element.
    fn repeated_i64(&mut self, wire_type: u64, out: &mut Vec<i64>) -> Decoded<()> {
        match wire_type {
            WT_LEN => {
                let mut inner = Reader::new(self.bytes()?);
                while !inner.done() {
                    out.push(inner.varint()? as i64);
                }
                Ok(())
            }
            WT_VARINT => {
                out.push(self.varint()? as i64);
                Ok(())
            }
            other => self.skip(other),
        }
    }
}

fn decode_level(buf: &[u8]) -> Decoded<Level> {
    let mut r = Reader::new(buf);
    let mut level = Level::default();
    while !r.done() {
        let (field, wt) = r.key()?;
        match (field, wt) {
            (1, WT_VARINT) => level.price = r.varint()? as i64,
            (2, WT_VARINT) => level.qty = r.varint()? as i64,
            (3, WT_VARINT) => level.orders = r.varint()? as i64,
            (_, other) => r.skip(other)?,
        }
    }
    Ok(level)
}

fn decode_tick(buf: &[u8]) -> Decoded<Tick> {
    let mut r = Reader::new(buf);
    let mut t = Tick::default();
    while !r.done() {
        let (field, wt) = r.key()?;
        if wt == WT_VARINT && (1..=10).contains(&field) {
            let v = r.varint()?;
            let signed = v as i64;
            match field {
                1 => t.instrument_id = signed,
                2 => t.exchange_segment = v,
                4 => t.last_trade_price = signed,
                5 => t.last_trade_time = Some(signed),
                6 => t.volume_today = signed,
                7 => t.dpr_upper = signed,
                8 => t.dpr_lower = signed,
                10 => t.open_interest = Some(signed),
                _ => {}
            }
        } else if wt == WT_LEN && (field == 11 || field == 12) {
            let level = decode_level(r.bytes()?)?;
            if field == 11 {
                t.best_buys.push(level);
            } else {
                t.best_sells.push(level);
            }
        } else {
            r.skip(wt)?;
        }
    }
    Ok(t)
}

fn decode_ack(buf: &[u8]) -> Decoded<Ack> {
    let mut r = Reader::new(buf);
    let mut ack = Ack::default();
    while !r.done() {
        let (field, wt) = r.key()?;
        match (field, wt) {
            (1, WT_VARINT) => ack.action = r.varint()?,
            (2, other) => r.repeated_i64(other, &mut ack.instrument_ids)?,
            (_, other) => r.skip(other)?,
        }
    }
    Ok(ack)
}

fn decode_error(buf: &[u8]) -> Decoded<StreamError> {
    let mut r = Reader::new(buf);
    let mut e = StreamError::default();
    while !r.done() {
        let (field, wt) = r.key()?;
        match (field, wt) {
            (1, WT_LEN) => e.code = String::from_utf8_lossy(r.bytes()?).into_owned(),
            (2, WT_LEN) => e.message = String::from_utf8_lossy(r.bytes()?).into_owned(),
            (_, other) => r.skip(other)?,
        }
    }
    Ok(e)
}

/// Decode one `ServerMessage` frame. For the oneof, the last payload field
/// on the wire wins, as protobuf specifies.
pub(crate) fn decode_server_message(data: &[u8]) -> Decoded<ServerMessage> {
    let mut r = Reader::new(data);
    let mut out = ServerMessage::Empty;
    while !r.done() {
        let (field, wt) = r.key()?;
        if wt != WT_LEN || !(1..=5).contains(&field) {
            r.skip(wt)?;
            continue;
        }
        let payload = r.bytes()?;
        out = match field {
            1 => ServerMessage::Ack(decode_ack(payload)?),
            2 => ServerMessage::Tick(TickKind::Ltp, decode_tick(payload)?),
            3 => ServerMessage::Tick(TickKind::Quote, decode_tick(payload)?),
            4 => ServerMessage::Tick(TickKind::Depth, decode_tick(payload)?),
            _ => ServerMessage::Error(decode_error(payload)?),
        };
    }
    Ok(out)
}
