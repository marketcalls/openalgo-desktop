//! The 127-entry indicator catalogue, ported from the web's
//! `services/agent/indicators/registry.py`: for each callable on `ta`, the
//! OHLCV series it takes, its parameters with the library's own defaults,
//! its outputs in return order, its measured warm-up and the caveats a
//! caller must know.
//!
//! The web builds `params` at import from `inspect.signature`; here they are
//! written out, and a test compares the whole table with the registry the
//! web renders over the openalgo build it pins
//! (`tests/fixtures/agent_indicators/registry.json`), so a renamed parameter
//! or a changed default is caught when the fixtures are recorded again.
//!
//! One deliberate deviation (lead decision, 1.0.4): `vi` and `ulcerindex`
//! are `ok` here. The web registers them broken, a verdict measured on
//! openalgo 2.0.3; the 2.0.5 build it pins computes both.
//!
//! The tables live in one file per category (`trend.rs` ... `utility.rs`).

mod hybrid;
mod momentum;
mod oscillators;
mod statistical;
mod talib_extra;
mod trend;
mod utility;
mod volatility;
mod volume;

/// The catalogue's categories, in the web's order.
pub(crate) const CATEGORIES: [&str; 9] = [
    "trend",
    "momentum",
    "volatility",
    "volume",
    "oscillators",
    "statistical",
    "hybrid",
    "talib_extra",
    "utility",
];

const C: &[&str] = &["close"];
const CV: &[&str] = &["close", "volume"];
const HL: &[&str] = &["high", "low"];
const HLC: &[&str] = &["high", "low", "close"];
const HLCV: &[&str] = &["high", "low", "close", "volume"];
const HLV: &[&str] = &["high", "low", "volume"];
const OHLC: &[&str] = &["open", "high", "low", "close"];
const V: &[&str] = &["volume"];
const NONE: &[&str] = &[];

/// A parameter's type, as the web's `ParamSpec.kind` names it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Kind {
    Int,
    Float,
    Str,
    Bool,
}

impl Kind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Kind::Int => "int",
            Kind::Float => "float",
            Kind::Str => "str",
            Kind::Bool => "bool",
        }
    }
}

/// The library's default for a parameter; `Required` when it has none.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Default {
    Required,
    Int(i64),
    Float(f64),
    Str(&'static str),
    Bool(bool),
}

/// One non-series argument of an indicator.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ParamSpec {
    pub name: &'static str,
    pub kind: Kind,
    pub default: Default,
    /// The only values the library accepts, when it validates any.
    pub allowed: &'static [&'static str],
}

impl ParamSpec {
    const fn new(name: &'static str, kind: Kind, default: Default) -> Self {
        Self {
            name,
            kind,
            default,
            allowed: &[],
        }
    }
    const fn required_int(name: &'static str) -> Self {
        Self::new(name, Kind::Int, Default::Required)
    }
    const fn int(name: &'static str, default: i64) -> Self {
        Self::new(name, Kind::Int, Default::Int(default))
    }
    const fn float(name: &'static str, default: f64) -> Self {
        Self::new(name, Kind::Float, Default::Float(default))
    }
    const fn text(name: &'static str, default: &'static str) -> Self {
        Self::new(name, Kind::Str, Default::Str(default))
    }
    const fn flag(name: &'static str, default: bool) -> Self {
        Self::new(name, Kind::Bool, Default::Bool(default))
    }
    const fn allowed(mut self, values: &'static [&'static str]) -> Self {
        self.allowed = values;
        self
    }

    /// True when the library declares no default.
    pub(crate) fn required(&self) -> bool {
        matches!(self.default, Default::Required)
    }
}

/// `ok`, or `alias` for a second name of another entry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Status {
    Ok,
    Alias,
}

impl Status {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Alias => "alias",
        }
    }
}

/// One callable on `ta`.
#[derive(Debug, Clone, Copy)]
pub struct IndicatorSpec {
    pub(crate) name: &'static str,
    pub(crate) category: &'static str,
    /// OHLCV columns in call order; empty for the pair helpers.
    pub(crate) inputs: &'static [&'static str],
    /// Output names in return order.
    pub(crate) outputs: &'static [&'static str],
    /// Non-series arguments in signature order.
    pub(crate) params: &'static [ParamSpec],
    pub(crate) status: Status,
    pub(crate) alias_of: Option<&'static str>,
    /// Bars of leading NaN before every output is finite, with the defaults.
    pub(crate) warmup: usize,
    pub(crate) note: &'static str,
    /// The signature's names for the series arguments, in order.
    pub(crate) series_args: &'static [&'static str],
    /// True for `beta` and `correlation`, which compare two instruments.
    pub(crate) needs_second_series: bool,
    /// `"param=value"` and the outputs that argument value produces instead.
    pub(crate) conditional_outputs: &'static [(&'static str, &'static [&'static str])],
}

impl IndicatorSpec {
    const BASE: IndicatorSpec = IndicatorSpec {
        name: "",
        category: "",
        inputs: NONE,
        outputs: &[],
        params: &[],
        status: Status::Ok,
        alias_of: None,
        warmup: 0,
        note: "",
        series_args: &[],
        needs_second_series: false,
        conditional_outputs: &[],
    };

    pub(crate) fn param(&self, name: &str) -> Option<&'static ParamSpec> {
        self.params.iter().find(|p| p.name == name)
    }
}

/// Every entry, category by category in the web's order.
pub(crate) fn all() -> impl Iterator<Item = &'static IndicatorSpec> {
    [
        trend::SPECS,
        momentum::SPECS,
        volatility::SPECS,
        volume::SPECS,
        oscillators::SPECS,
        statistical::SPECS,
        hybrid::SPECS,
        talib_extra::SPECS,
        utility::SPECS,
    ]
    .into_iter()
    .flatten()
}

/// How many entries the catalogue holds (127).
pub(crate) fn count() -> usize {
    all().count()
}

/// One entry by its exact `ta` name, already lower-cased and trimmed.
pub fn get_spec(name: &str) -> Option<&'static IndicatorSpec> {
    all().find(|spec| spec.name == name)
}

/// How many entries each category holds, in category order.
pub(crate) fn category_counts() -> Vec<(&'static str, usize)> {
    CATEGORIES
        .iter()
        .map(|category| (*category, all().filter(|s| s.category == *category).count()))
        .collect()
}

/// `REQUIRED_PARAM_DEFAULTS` (`services/indicator_service.py`): the
/// parameter with no library default, and the value it takes when the caller
/// leaves it out.
pub(crate) fn required_default(name: &str) -> Option<(&'static str, i64)> {
    match name {
        "sma" | "ema" | "wma" | "hma" | "dema" | "tema" | "zlema" | "vwma" | "highest"
        | "lowest" | "stdev" => Some(("period", 14)),
        "roc" => Some(("length", 14)),
        "rising" | "falling" => Some(("length", 1)),
        _ => None,
    }
}
