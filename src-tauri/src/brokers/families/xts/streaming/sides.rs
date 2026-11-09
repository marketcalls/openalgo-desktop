//! RMoney's books one side at a time (audit R107-09): RMoney's web adapter
//! makes a book from either side alone, and a 1502 may name one side and
//! not the other. A side it names replaces that side (an empty one clears
//! it); a side it names nowhere is not a cleared one, so the instrument's
//! last side stands (bid and ask are market state: they go only on an
//! explicit clear). The bridge keeps whole books only, so the feed keeps
//! each instrument's last two sides and completes a one-sided book from
//! them. A side never seen in this connection is empty. Kept per
//! connection (cleared on connect) and per subscription (dropped when the
//! instrument is let go).

use super::super::mapping;
use serde_json::Value;
use std::collections::HashMap;

/// An instrument on the feed: `(segment code, instrument id)`.
pub(super) type Route = (i64, String);

/// The instrument a message (in the template's shape) is for.
pub(super) fn route(v: &Value) -> Route {
    let id = mapping::s(v, "ExchangeInstrumentID");
    (mapping::i(v, "ExchangeSegment"), id.trim().to_string())
}

/// Each held instrument's last bid and ask sides, as lists of levels.
#[derive(Default)]
pub(super) struct KeptSides(HashMap<Route, [Value; 2]>);

impl KeptSides {
    /// A 1502 `v` (in the template's shape) with a side it names nowhere
    /// taken from `route`'s last; the sides it names are kept for the next.
    /// Another message, or one naming neither side, is left as it is.
    pub(super) fn complete(&mut self, route: Route, v: &mut Value) {
        if mapping::i(v, "MessageCode") != 1502 {
            return;
        }
        let Some(o) = v.as_object_mut() else {
            return;
        };
        let named = [o.get("Bids").cloned(), o.get("Asks").cloned()];
        if named.iter().all(Option::is_none) {
            return;
        }
        let kept = self.0.entry(route).or_insert_with(|| [empty(), empty()]);
        for (side, sent) in kept.iter_mut().zip(named) {
            if let Some(levels) = sent {
                *side = levels;
            }
        }
        o.insert("Bids".into(), kept[0].clone());
        o.insert("Asks".into(), kept[1].clone());
    }

    /// The instrument was let go: its sides go with it.
    pub(super) fn forget(&mut self, route: &Route) {
        self.0.remove(route);
    }

    /// A new connection starts every book afresh.
    pub(super) fn clear(&mut self) {
        self.0.clear();
    }
}

fn empty() -> Value {
    Value::Array(Vec::new())
}
