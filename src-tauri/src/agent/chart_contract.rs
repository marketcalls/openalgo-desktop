//! The `/trading` chart wire contract, in both directions (the web's
//! `services/agent/chart_contract.py`).
//!
//! Inbound is [`ChartView`], what the Assistant panel sends about the chart
//! the trader is looking at. Outbound is [`Command`], the vocabulary the
//! Agent sends back on a `chart_command` frame. Both live here so the panel,
//! the chart tools and a test read one file.
//!
//! Outbound: three ops, and that is the whole vocabulary. `draw` replaces
//! one Agent group (so asking twice for levels redraws rather than stacks),
//! `clear` removes one Agent group or every one (`group: null`), and
//! `indicator` adds or removes an indicator by its chart id. The terminal
//! ignores an op it does not know, so the vocabulary can grow without a
//! lockstep release.
//!
//! Grouping is what makes a clear safe: every shape is drawn under
//! `ai:{group}:{index}`, assigned here by [`draw`] and nowhere else, and the
//! terminal's clear removes only ids with that prefix. A drawing the trader
//! placed by hand never carries it. Shapes carry a semantic tone, never a
//! colour: the chart resolves it against the active theme.
//!
//! Provenance: a shape's anchors are `{time, price}` from candles the
//! platform returned. The one string a tool passes through from the model,
//! a caption, goes through [`safe_note`] first, which removes every digit.
//!
//! Inbound: the context reaches the prompt on every turn, so every field
//! [`ChartView`] models is bounded here (scalars capped and folded to one
//! line, lists capped in count and depth) and anything it does not model is
//! dropped. The session bullets are rendered by `chat::request`; the lists
//! reach only the chart tools.

use crate::agent::tools::numbers::{as_number, py_round};
use crate::agent::tools::values::py_str;
use serde::Serialize;
use serde_json::{Map, Value};

/// Marks a drawing as the Agent's. The terminal splits an id on `:`, so a
/// group name never contains one.
pub const AI_PREFIX: &str = "ai";

/// Most shapes one group may carry.
pub const MAX_SHAPES: usize = 24;

/// Longest caption a tool may pass through from the model.
pub const MAX_NOTE_CHARS: usize = 48;

/// Longest label or marker text drawn with a shape.
const MAX_LABEL_CHARS: usize = 80;

/// Most indicators one context may report.
pub const MAX_INDICATORS: usize = 12;

/// Most trader drawings one context may report.
pub const MAX_DRAWINGS: usize = 24;

/// Anchors kept per trader drawing: two place a line or a rectangle, four a
/// channel or a fib; a freehand path's first anchors are enough to place it.
pub const MAX_DRAWING_POINTS: usize = 4;

/// The closed set of groups the Agent draws into. A clear with no group
/// removes exactly these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    Levels,
    Trendline,
    Zone,
    Patterns,
}

impl Group {
    /// Every group, in the web's `GROUPS` order.
    pub const ALL: [Group; 4] = [
        Group::Levels,
        Group::Trendline,
        Group::Zone,
        Group::Patterns,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Group::Levels => "levels",
            Group::Trendline => "trendline",
            Group::Zone => "zone",
            Group::Patterns => "patterns",
        }
    }

    /// A group by name, case and surrounding spaces ignored; `None` for a
    /// name outside the set (the web's `ValueError`: a shape drawn into a
    /// group no clear knows could never be removed).
    pub fn parse(name: &str) -> Option<Group> {
        let wanted = name.trim().to_lowercase();
        Group::ALL.into_iter().find(|g| g.as_str() == wanted)
    }
}

/// The semantic tones the chart resolves against its theme (`TONES`).
pub const TONES: [&str; 3] = ["bullish", "bearish", "neutral"];

/// A tone the chart knows, else `neutral` (`_tone`).
pub fn tone(value: &str) -> &'static str {
    let wanted = value.trim().to_lowercase();
    TONES
        .into_iter()
        .find(|t| *t == wanted)
        .unwrap_or("neutral")
}

/// Python's `str.strip(chars)` for a set of ASCII characters.
fn strip_chars<'a>(text: &'a str, chars: &str) -> &'a str {
    text.trim_matches(|c: char| chars.contains(c))
}

/// Collapse every run of white space to one space (`_WHITESPACE.sub`).
fn collapse_spaces(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Reduce a model-supplied caption to something safe to draw: every digit
/// and every character outside letters, spaces and `,.:;()/'-` removed,
/// white space collapsed, capped at [`MAX_NOTE_CHARS`] (`safe_note`).
/// Anything that is not a string reads as no caption.
pub fn safe_note(value: &Value) -> String {
    let Value::String(raw) = value else {
        return String::new();
    };
    let allowed = |c: char| c.is_ascii_alphabetic() || " ,.:;()/'-".contains(c);
    let replaced: String = raw
        .chars()
        .map(|c| if allowed(c) { c } else { ' ' })
        .collect();
    let collapsed = collapse_spaces(&replaced);
    let cut: String = strip_chars(&collapsed, " ,.:;-")
        .chars()
        .take(MAX_NOTE_CHARS)
        .collect();
    cut.trim().to_string()
}

/// One point in data space: UTC epoch seconds and a price on the pane's
/// scale, rounded to four places.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Anchor {
    pub time: f64,
    pub price: f64,
}

impl Anchor {
    /// `None` when either value is not finite (`anchor`).
    pub fn new(time: f64, price: f64) -> Option<Anchor> {
        (time.is_finite() && price.is_finite()).then(|| Anchor {
            time,
            price: py_round(price, 4),
        })
    }

    /// An anchor from a `{time, price}` object (`_as_anchor`).
    fn from_value(value: &Value) -> Option<Anchor> {
        let obj = value.as_object()?;
        Anchor::new(
            as_number(obj.get("time")?)?,
            as_number(obj.get("price").unwrap_or(&Value::Null))?,
        )
    }
}

/// One shape the Agent draws. Built only by the constructors below, which
/// return `None` for an unusable anchor so the shape is dropped.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Shape {
    /// A horizontal level; with a start time, a ray running right.
    Level {
        price: f64,
        tone: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        time: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ray: Option<bool>,
        #[serde(skip_serializing_if = "String::is_empty")]
        label: String,
    },
    /// A straight line between two anchors, optionally projected right.
    Trendline {
        from: Anchor,
        to: Anchor,
        extend_right: bool,
        tone: &'static str,
        #[serde(skip_serializing_if = "String::is_empty")]
        label: String,
    },
    /// A filled rectangle between two corners.
    Zone {
        from: Anchor,
        to: Anchor,
        tone: &'static str,
        #[serde(skip_serializing_if = "String::is_empty")]
        label: String,
    },
    /// A labelled dot on one bar.
    Marker {
        at: Anchor,
        text: String,
        tone: &'static str,
    },
}

fn label(text: &str) -> String {
    text.chars().take(MAX_LABEL_CHARS).collect()
}

/// A horizontal support or resistance line (`level`).
pub fn level(price: f64, time: Option<f64>, text: &str, tone_name: &str) -> Option<Shape> {
    if !price.is_finite() {
        return None;
    }
    let time = time.filter(|t| t.is_finite());
    Some(Shape::Level {
        price: py_round(price, 4),
        tone: tone(tone_name),
        time,
        ray: time.map(|_| true),
        label: label(text),
    })
}

/// A line between two anchors (`trendline`).
pub fn trendline(
    from: Option<Anchor>,
    to: Option<Anchor>,
    extend_right: bool,
    text: &str,
    tone_name: &str,
) -> Option<Shape> {
    Some(Shape::Trendline {
        from: from?,
        to: to?,
        extend_right,
        tone: tone(tone_name),
        label: label(text),
    })
}

/// A filled band between two corners (`zone`).
pub fn zone(
    from: Option<Anchor>,
    to: Option<Anchor>,
    text: &str,
    tone_name: &str,
) -> Option<Shape> {
    Some(Shape::Zone {
        from: from?,
        to: to?,
        tone: tone(tone_name),
        label: label(text),
    })
}

/// A labelled dot on one bar (`marker`).
pub fn marker(at: Option<Anchor>, text: &str, tone_name: &str) -> Option<Shape> {
    Some(Shape::Marker {
        at: at?,
        text: label(text),
        tone: tone(tone_name),
    })
}

/// The id one shape is drawn under: `ai:{group}:{index}`.
pub fn drawing_id(group: Group, index: usize) -> String {
    format!("{}:{}:{}", AI_PREFIX, group.as_str(), index)
}

/// A shape with the id [`draw`] gave it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Drawn {
    #[serde(flatten)]
    pub shape: Shape,
    pub id: String,
}

/// What an indicator command asks of the chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IndicatorAction {
    Add,
    Remove,
}

/// One chart command (a `chart_command` frame carries a list of them).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Command {
    /// Replace one Agent group with these shapes; none clears it.
    Draw { group: Group, shapes: Vec<Drawn> },
    /// Remove one Agent group, or every one when `group` is null.
    Clear { group: Option<Group> },
    /// Add or remove an indicator by its chart id (built in the chart
    /// tools, as the web builds it in `tools/chart.py`).
    Indicator {
        action: IndicatorAction,
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        settings: Option<Map<String, Value>>,
    },
}

impl Command {
    /// The command as it crosses the wire.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// A draw command replacing `group` (`draw`): `None` shapes dropped, the
/// rest id'd by their position, at most [`MAX_SHAPES`].
pub fn draw(group: Group, shapes: impl IntoIterator<Item = Option<Shape>>) -> Command {
    let shapes = shapes
        .into_iter()
        .flatten()
        .take(MAX_SHAPES)
        .enumerate()
        .map(|(index, shape)| Drawn {
            shape,
            id: drawing_id(group, index),
        })
        .collect();
    Command::Draw { group, shapes }
}

/// A clear command (`clear`): one group, or every Agent group for `None`.
/// Never touches a drawing the trader placed.
pub fn clear(group: Option<Group>) -> Command {
    Command::Clear { group }
}

/// Which Agent groups are on the chart once `commands` land (the web's
/// `tools/chart.py` `_groups_after`), in order of first appearance: a draw
/// with shapes puts its group on, a draw with none takes it off, a clear
/// removes its group or all of them. Only the Agent's own groups.
pub fn groups_after(groups: &[Group], commands: &[Command]) -> Vec<Group> {
    let mut on_screen: Vec<Group> = Vec::new();
    for g in groups {
        if !on_screen.contains(g) {
            on_screen.push(*g);
        }
    }
    for command in commands {
        match command {
            Command::Draw { group, shapes } if !shapes.is_empty() => {
                if !on_screen.contains(group) {
                    on_screen.push(*group);
                }
            }
            Command::Draw { group, .. } | Command::Clear { group: Some(group) } => {
                on_screen.retain(|g| g != group)
            }
            Command::Clear { group: None } => on_screen.clear(),
            Command::Indicator { .. } => {}
        }
    }
    on_screen
}

// ---------------------------------------------------------------------------
// Inbound: the chart the trader is looking at
// ---------------------------------------------------------------------------

/// Python's `str.isprintable` for one character, as near as the standard
/// library allows: control, format and separator characters (other than
/// the ASCII space) are not printable.
fn printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    let format = matches!(c,
        '\u{00AD}' | '\u{0600}'..='\u{0605}' | '\u{061C}' | '\u{06DD}' | '\u{070F}'
        | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206F}' | '\u{FEFF}'
        | '\u{FFF9}'..='\u{FFFB}' | '\u{E000}'..='\u{F8FF}');
    !(c.is_control() || c.is_whitespace() || format)
}

/// One bounded scalar (`_text`): at most `limit` characters on one line,
/// control characters and line breaks folded to spaces, because the value
/// is about to be rendered as a prompt bullet.
fn text(value: Option<&Value>, limit: usize, upper: bool) -> String {
    let raw = match value {
        None | Some(Value::Null) => String::new(),
        Some(v) => py_str(v),
    };
    let folded: String = raw
        .chars()
        .map(|c| if printable(c) { c } else { ' ' })
        .collect();
    let cut: String = collapse_spaces(&folded).chars().take(limit).collect();
    if upper {
        cut.to_uppercase()
    } else {
        cut
    }
}

/// A non-negative count (`_count`).
fn count(value: Option<&Value>) -> u64 {
    value
        .and_then(as_number)
        .map(|n| n.trunc().max(0.0) as u64)
        .unwrap_or(0)
}

/// Python's truthiness for the `indicatorId or id` fallback.
fn truthy(value: Option<&Value>) -> Option<&Value> {
    value.filter(|v| crate::agent::tools::values::truthy(v))
}

/// An indicator on the chart: `id` is the descriptor the chart registry
/// knows (`ema`), the only id an indicator tool can act on.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IndicatorOnChart {
    pub id: String,
    pub name: String,
}

/// A drawing the trader placed by hand. The panel leaves the Agent's own
/// drawings out, knowing them by their `ai:` prefix.
#[derive(Debug, Clone, PartialEq)]
pub struct TraderDrawing {
    pub tool: String,
    pub points: Vec<Anchor>,
    pub text: String,
}

/// What the panel reports about the chart, read fresh on every message
/// (`ChartView`). [`ChartView::from_context`] is the only way to build one
/// from the wire and is where every cap is applied.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChartView {
    pub symbol: String,
    pub exchange: String,
    pub interval: String,
    pub chart_type: String,
    pub bars_loaded: u64,
    pub visible_bars: u64,
    pub visible_from: Option<f64>,
    pub visible_to: Option<f64>,
    pub last_price: Option<f64>,
    pub indicators: Vec<IndicatorOnChart>,
    pub drawings: Vec<TraderDrawing>,
    /// Agent groups still on the chart: the trader can clear the Agent's
    /// markup from the toolbar without any tool being called.
    pub agent_groups: Vec<Group>,
}

fn items(value: Option<&Value>) -> &[Value] {
    value.and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

impl ChartView {
    /// A symbol, an exchange and an interval are all present: the least a
    /// tool needs to fetch the right candles.
    pub fn is_open(&self) -> bool {
        !self.symbol.is_empty() && !self.exchange.is_empty() && !self.interval.is_empty()
    }

    /// A bounded view of whatever the panel sent (`from_payload`); no
    /// context, or one that is not an object, is a closed view.
    pub fn from_context(context: Option<&Map<String, Value>>) -> ChartView {
        let Some(p) = context else {
            return ChartView::default();
        };
        ChartView {
            symbol: text(p.get("symbol"), 64, true),
            exchange: text(p.get("exchange"), 16, true),
            interval: text(p.get("interval"), 16, false),
            chart_type: text(p.get("chart_type"), 24, false),
            bars_loaded: count(p.get("bars_loaded")),
            visible_bars: count(p.get("visible_bars")),
            visible_from: p.get("visible_from").and_then(as_number),
            visible_to: p.get("visible_to").and_then(as_number),
            last_price: p.get("last_price").and_then(as_number),
            indicators: indicators_of(p.get("indicators")),
            drawings: drawings_of(p.get("drawings")),
            agent_groups: groups_of(p.get("agent_groups")),
        }
    }
}

/// `indicatorId` (the descriptor) is preferred over `id` (the instance,
/// `ema-1`, unique so three EMAs can coexist); older panels send only `id`.
fn indicators_of(value: Option<&Value>) -> Vec<IndicatorOnChart> {
    items(value)
        .iter()
        .filter_map(Value::as_object)
        .filter_map(|item| {
            let name = text(item.get("name"), 48, false);
            if name.is_empty() {
                return None;
            }
            let descriptor = truthy(item.get("indicatorId")).or_else(|| item.get("id"));
            Some(IndicatorOnChart {
                id: text(descriptor, 48, false),
                name,
            })
        })
        .take(MAX_INDICATORS)
        .collect()
}

fn drawings_of(value: Option<&Value>) -> Vec<TraderDrawing> {
    items(value)
        .iter()
        .filter_map(Value::as_object)
        .take(MAX_DRAWINGS)
        .map(|item| {
            let mut points = Vec::new();
            for raw in items(item.get("points")) {
                points.extend(Anchor::from_value(raw));
                if points.len() >= MAX_DRAWING_POINTS {
                    break;
                }
            }
            TraderDrawing {
                tool: text(item.get("tool"), 32, false),
                points,
                text: text(item.get("text"), 80, false),
            }
        })
        .collect()
}

fn groups_of(value: Option<&Value>) -> Vec<Group> {
    let mut groups = Vec::new();
    for raw in items(value) {
        let name = text(Some(raw), 16, false).to_lowercase();
        if let Some(g) = Group::ALL.into_iter().find(|g| g.as_str() == name) {
            if !groups.contains(&g) {
                groups.push(g);
            }
        }
    }
    groups
}

#[cfg(test)]
mod tests;
