//! InvestRight market-data feed messages (web
//! `streaming/hdfcsecurities_market_pb2.py`, package `hdfcsecurities`).
//!
//! Hand-written prost messages with the field numbers and types of the
//! serialized descriptor in that file. Only the market-data part is
//! modelled; `Order`, `Trade` and the other messages the schema declares are
//! skipped as unknown fields, which protobuf decoding allows.

#![allow(clippy::derive_partial_eq_without_eq)]

/// `GenericDTOList`.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct GenericDtoList {
    #[prost(message, repeated, tag = "1")]
    pub generic_dto_list: ::prost::alloc::vec::Vec<GenericDto>,
}

/// `GenericDTO` (fields 5 `order` and 6 `trade` are not modelled).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct GenericDto {
    #[prost(int64, tag = "1")]
    pub instrument_id: i64,
    #[prost(message, optional, tag = "2")]
    pub mbp_data: ::core::option::Option<MbpData>,
    #[prost(message, optional, tag = "3")]
    pub index_data: ::core::option::Option<IndexData>,
    /// `MarketStatus` enum.
    #[prost(int32, tag = "4")]
    pub market_status: i32,
    #[prost(int64, tag = "7")]
    pub sequence_no: i64,
    /// Epoch milliseconds when set.
    #[prost(int64, tag = "8")]
    pub packet_timestamp: i64,
    /// `PacketType` enum (see `packet_type`).
    #[prost(int32, tag = "9")]
    pub packet_type: i32,
    #[prost(message, optional, tag = "10")]
    pub greek_data: ::core::option::Option<GreekData>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct GreekData {
    #[prost(double, tag = "1")]
    pub delta: f64,
    #[prost(double, tag = "2")]
    pub gamma: f64,
    #[prost(double, tag = "3")]
    pub vega: f64,
    #[prost(double, tag = "4")]
    pub theta: f64,
    #[prost(double, tag = "5")]
    pub rho: f64,
    #[prost(string, tag = "6")]
    pub scrip_id: ::prost::alloc::string::String,
    #[prost(string, tag = "7")]
    pub exch: ::prost::alloc::string::String,
}

/// `MBPData`: prices are rupees (doubles), no scaling.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct MbpData {
    #[prost(double, tag = "1")]
    pub last_traded_price: f64,
    #[prost(int64, tag = "2")]
    pub last_trade_time: i64,
    #[prost(string, tag = "3")]
    pub net_change_indicator: ::prost::alloc::string::String,
    #[prost(int32, tag = "4")]
    pub net_price_change_from_closing_price: i32,
    #[prost(double, tag = "5")]
    pub open_price: f64,
    #[prost(double, tag = "6")]
    pub high_price: f64,
    #[prost(double, tag = "7")]
    pub closing_price: f64,
    #[prost(double, tag = "8")]
    pub low_price: f64,
    #[prost(int64, tag = "9")]
    pub volume_traded_today: i64,
    #[prost(int64, tag = "10")]
    pub last_trade_quantity: i64,
    #[prost(double, tag = "11")]
    pub average_trade_price: f64,
    #[prost(message, optional, tag = "12")]
    pub market_depth_dto_list: ::core::option::Option<MarketDepthDtoList>,
    #[prost(int64, tag = "13")]
    pub total_buy_quantity: i64,
    #[prost(int64, tag = "14")]
    pub total_sell_quantity: i64,
    #[prost(double, tag = "15")]
    pub lower_circuit_limit: f64,
    #[prost(double, tag = "16")]
    pub upper_circuit_limit: f64,
    #[prost(int64, tag = "17")]
    pub oi: i64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct IndexData {
    #[prost(string, tag = "1")]
    pub index_name: ::prost::alloc::string::String,
    #[prost(double, tag = "2")]
    pub index_value: f64,
    #[prost(double, tag = "3")]
    pub high_index_value: f64,
    #[prost(double, tag = "4")]
    pub low_index_value: f64,
    #[prost(double, tag = "5")]
    pub opening_index: f64,
    #[prost(double, tag = "6")]
    pub closing_index: f64,
    #[prost(double, tag = "7")]
    pub percent_change: f64,
    #[prost(double, tag = "8")]
    pub yearly_high: f64,
    #[prost(double, tag = "9")]
    pub yearly_low: f64,
    #[prost(int64, tag = "10")]
    pub packet_time_stamp: i64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct MarketDepthDtoList {
    #[prost(message, repeated, tag = "1")]
    pub market_depth_dto: ::prost::alloc::vec::Vec<MarketDepthDto>,
}

#[derive(Clone, Copy, PartialEq, ::prost::Message)]
pub struct MarketDepthDto {
    #[prost(int64, tag = "1")]
    pub quantity: i64,
    #[prost(double, tag = "2")]
    pub price: f64,
    #[prost(int64, tag = "3")]
    pub number_of_orders: i64,
    /// true on the bid side.
    #[prost(bool, tag = "4")]
    pub buy_flag: bool,
}

/// `PacketType` enum values.
pub mod packet_type {
    pub const NSE_CM_ALL: i32 = 0;
    pub const NSE_CD_ALL: i32 = 1;
    pub const NSE_INDEX: i32 = 2;
    pub const NSE_FO_ALL: i32 = 3;
    pub const BSE_CM: i32 = 4;
    pub const BSE_INDEX: i32 = 5;
    pub const BSE_FO_ALL: i32 = 6;
    pub const MCX_PKT: i32 = 7;
    pub const ORDER: i32 = 8;
    pub const TRADE: i32 = 9;
    pub const NSE_CM_CIRC: i32 = 10;
    pub const NSE_CD_CIRC: i32 = 11;
    pub const NSE_CD_OI: i32 = 12;
    pub const NSE_FO_CIRC: i32 = 13;
    pub const NSE_FO_OI: i32 = 14;
    pub const BSE_FO_OI: i32 = 15;
    pub const HEARTBEAT: i32 = 16;
    pub const NSE_FO_GREEK: i32 = 17;
    pub const BSE_FO_GREEK: i32 = 18;
}
