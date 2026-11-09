//! What the web's 8765 server sends each client, from `web.json`
//! (review R107-13): `scripts/feed_corpus/web_expected.py` runs the web
//! server's own subscription and delivery methods (`webserver.py`) in front
//! of the broker's web adapter, and records, for every step that made the
//! adapter publish (a delivery point), each frame a client gets: the
//! publication it came from, the mode it is tagged with, the publication's
//! mode, and the client's held modes it answers (`serves`).
//!
//! The desktop sends one frame per held mode (C08), so a client's frame at
//! a mode consumes the next delivery that serves that mode; a delivery is
//! consumed at most once per mode, and every delivery that serves a held
//! mode must be consumed by a frame or declared by a `silence` step naming
//! it before the next delivery point. The web's fan-out to a lower-mode
//! holder that serves none of its held modes may go unconsumed (the
//! approved `one-frame-per-held-mode` rule), though a frame may still pair
//! with it when nothing serves its mode.

use super::case::bad;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// One frame the web sends one client.
#[derive(Debug, Clone)]
pub(crate) struct Delivery {
    pub(crate) client: String,
    pub(crate) publication: usize,
    /// The publication's mode (market data); `None` for an order update.
    topic_mode: Option<u64>,
    /// The held modes it answers; for an order update, the update itself.
    serves: Vec<u64>,
}

/// A delivery point: one step's publications and what each client got.
#[derive(Debug, Clone)]
pub(crate) struct Point {
    pub(crate) published: Vec<Value>,
    pub(crate) deliveries: Vec<Delivery>,
}

/// An order update's place in `serves`.
const ORDER: u64 = 0;

/// The delivery points of a `web.json`, by script line.
pub(crate) fn points(at: &str, web: &Value) -> BTreeMap<usize, Point> {
    let line_of = |v: &Value| {
        v.as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or_else(|| bad(at, "a delivery point without its script line"))
    };
    web["pushes"]
        .as_array()
        .unwrap_or_else(|| bad(at, "no \"pushes\""))
        .iter()
        .map(|p| {
            let published = p["published"]
                .as_array()
                .cloned()
                .unwrap_or_else(|| bad(at, "a point without \"published\""));
            let deliveries = p["deliveries"]
                .as_array()
                .unwrap_or_else(|| {
                    bad(
                        at,
                        "a point without \"deliveries\": run scripts/feed_corpus/web_expected.py again",
                    )
                })
                .iter()
                .map(|d| delivery(at, d, published.len()))
                .collect();
            (line_of(&p["line"]), Point { published, deliveries })
        })
        .collect()
}

fn delivery(at: &str, d: &Value, publications: usize) -> Delivery {
    let publication = d["publication"]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n < publications)
        .unwrap_or_else(|| bad(at, format!("a delivery of no publication: {}", d)));
    let order = d.get("order").is_some_and(|o| o == true);
    let serves = d["serves"]
        .as_array()
        .unwrap_or_else(|| bad(at, format!("a delivery without \"serves\": {}", d)))
        .iter()
        .map(|s| match (s.as_u64(), s.as_str()) {
            (Some(m), _) => m,
            (_, Some("order")) => ORDER,
            _ => bad(at, format!("a delivery serving {}", s)),
        })
        .collect();
    Delivery {
        client: d["client"].as_str().unwrap_or_default().to_string(),
        publication,
        topic_mode: if order {
            None
        } else {
            d["topic_mode"].as_u64()
        },
        serves,
    }
}

/// One delivery point's deliveries, consumed by the case's steps.
pub(crate) struct Consumer<'a> {
    pub(crate) line: usize,
    point: &'a Point,
    /// (delivery, what consumed it): a held mode, an order, or a fold pair.
    used: BTreeSet<(usize, String)>,
}

impl<'a> Consumer<'a> {
    pub(crate) fn new(line: usize, point: &'a Point) -> Consumer<'a> {
        Consumer {
            line,
            point,
            used: BTreeSet::new(),
        }
    }

    fn publication(&self, d: &Delivery) -> &'a Value {
        &self.point.published[d.publication]
    }

    /// The publication a client's market frame at `mode` answers: the next
    /// delivery serving that held mode, else (the web's fan-out beyond the
    /// modes held) the next one of that mode, or of the lowest richer one.
    pub(crate) fn market(
        &mut self,
        client: &str,
        symbol: &str,
        exchange: &str,
        mode: u64,
    ) -> Option<&'a Value> {
        let point = self.point;
        let ds: Vec<(usize, &Delivery)> = point
            .deliveries
            .iter()
            .enumerate()
            .filter(|(_, d)| d.client == client && d.topic_mode.is_some())
            .filter(|(_, d)| {
                let p = &point.published[d.publication];
                p["symbol"] == symbol && p["exchange"] == exchange
            })
            .collect();
        let held = format!("m{}", mode);
        if let Some((i, d)) = ds
            .iter()
            .find(|(i, d)| d.serves.contains(&mode) && !self.used.contains(&(*i, held.clone())))
        {
            self.used.insert((*i, held));
            return Some(self.publication(d));
        }
        let fold = format!("fold{}", mode);
        let (i, d) = ds
            .iter()
            .filter(|(i, d)| {
                d.topic_mode.is_some_and(|t| t >= mode) && !self.used.contains(&(*i, fold.clone()))
            })
            .min_by_key(|(i, d)| (d.topic_mode != Some(mode), d.topic_mode, *i))?;
        self.used.insert((*i, fold));
        Some(self.publication(d))
    }

    /// The order update a client's frame answers: the next one to that
    /// client with the same order id, else the next one to that client.
    pub(crate) fn order(&mut self, client: &str, orderid: &Value) -> Option<&'a Value> {
        let point = self.point;
        let ds: Vec<(usize, &Delivery)> = point
            .deliveries
            .iter()
            .enumerate()
            .filter(|(i, d)| {
                d.client == client
                    && d.topic_mode.is_none()
                    && !self.used.contains(&(*i, "order".to_string()))
            })
            .collect();
        let (i, d) = ds
            .iter()
            .find(|(_, d)| point.published[d.publication]["frame"]["orderid"] == *orderid)
            .or_else(|| ds.first())?;
        self.used.insert((*i, "order".to_string()));
        Some(self.publication(d))
    }

    /// Every delivery that answers a held mode (or is an order update) and
    /// that no step accounted for. One web frame may answer several held
    /// modes (the desktop sends a frame per held mode), but it is sent
    /// once, so one of them accounts for it.
    pub(crate) fn unconsumed(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (i, d) in self.point.deliveries.iter().enumerate() {
            let consumed = self.used.iter().any(|(j, how)| {
                *j == i && (how == "order" || d.serves.iter().any(|m| *how == format!("m{}", m)))
            });
            if d.serves.is_empty() || consumed {
                continue;
            }
            let p = self.publication(d);
            let what = match d.topic_mode {
                None => format!("the order update {}", p["frame"]["orderid"]),
                Some(t) => format!(
                    "{} {} (publication {}, topic mode {}, serving held modes {:?}): {}",
                    p["exchange"], p["symbol"], d.publication, t, d.serves, p["data"]
                ),
            };
            out.push(format!(
                "after line {} the web sends client {} {}",
                self.line, d.client, what
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Two Kite quote packets for one instrument in one frame (count 2):
    /// the web publishes 1500.0 then 1501.0, both to the Quote holder.
    fn two_quotes() -> Point {
        let web = json!({"pushes": [{
            "line": 3,
            "published": [
                {"kind": "market", "symbol": "INFY", "exchange": "NSE", "mode": 2, "data": {"ltp": 1500.0}},
                {"kind": "market", "symbol": "INFY", "exchange": "NSE", "mode": 2, "data": {"ltp": 1501.0}}
            ],
            "deliveries": [
                {"client": "a", "publication": 0, "mode": 2, "topic_mode": 2, "serves": [2]},
                {"client": "a", "publication": 1, "mode": 2, "topic_mode": 2, "serves": [2]}
            ]
        }]});
        points("t", &web).remove(&3).unwrap()
    }

    /// Review R107-13's probe: each web publication is consumed once, in
    /// order, so a second frame repeating the first price is compared with
    /// the second publication (1501.0), never the first again.
    #[test]
    fn each_web_frame_is_consumed_once_in_order() {
        let point = two_quotes();
        let mut c = Consumer::new(3, &point);
        let first = c.market("a", "INFY", "NSE", 2).unwrap();
        let second = c.market("a", "INFY", "NSE", 2).unwrap();
        assert_eq!(first["data"]["ltp"], json!(1500.0));
        assert_eq!(second["data"]["ltp"], json!(1501.0));
        assert!(c.market("a", "INFY", "NSE", 2).is_none());
        assert!(c.unconsumed().is_empty());
    }

    /// A web frame no step accounts for fails the case.
    #[test]
    fn an_unconsumed_web_frame_is_named() {
        let point = two_quotes();
        let mut c = Consumer::new(3, &point);
        c.market("a", "INFY", "NSE", 2);
        let left = c.unconsumed();
        assert_eq!(left.len(), 1, "{:?}", left);
        assert!(left[0].contains("publication 1"), "{:?}", left);
    }

    /// The web's fan-out to a lower-mode holder that serves none of its
    /// held modes may go unconsumed; an order update pairs by its id.
    #[test]
    fn fan_out_beyond_held_modes_is_not_required_and_orders_pair_by_id() {
        let web = json!({"pushes": [{
            "line": 5,
            "published": [
                {"kind": "market", "symbol": "INFY", "exchange": "NSE", "mode": 2, "data": {"ltp": 1.0}},
                {"kind": "market", "symbol": "INFY", "exchange": "NSE", "mode": 1, "data": {"ltp": 1.0}},
                {"kind": "order", "frame": {"orderid": "A1"}},
                {"kind": "order", "frame": {"orderid": "B2"}}
            ],
            "deliveries": [
                {"client": "a", "publication": 0, "mode": 1, "topic_mode": 2, "serves": []},
                {"client": "a", "publication": 1, "mode": 1, "topic_mode": 1, "serves": [1]},
                {"client": "a", "publication": 2, "order": true, "serves": ["order"]},
                {"client": "a", "publication": 3, "order": true, "serves": ["order"]}
            ]
        }]});
        let point = points("t", &web).remove(&5).unwrap();
        let mut c = Consumer::new(5, &point);
        assert_eq!(c.market("a", "INFY", "NSE", 1).unwrap()["mode"], json!(1));
        assert_eq!(
            c.order("a", &json!("B2")).unwrap()["frame"]["orderid"],
            json!("B2")
        );
        let left = c.unconsumed();
        assert_eq!(left.len(), 1, "{:?}", left);
        assert!(left[0].contains("\"A1\""), "{:?}", left);
    }
}
