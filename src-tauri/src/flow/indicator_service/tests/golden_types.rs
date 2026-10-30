//! Every parameter given values of every type the node's params JSON can
//! carry (`types.json`, IND-042): fractions, numbers written as text, text
//! that is no number, null, lists, objects, booleans, and text parameters
//! given numbers, with each mode of `obv_smoothed`, `variance` and
//! `pvi_with_signal`. Where the web computes, the latest values match;
//! where it raises, the exception and its sentence match word for word.

use serde_json::{json, Value};

use super::support::{diff, error_diff, json as fixture, pairs, records, Failures, TYPES};
use crate::flow::indicator_service::compute::compute_indicator_as_web;
use crate::flow::indicator_service::{IndicatorParams, IndicatorRequest, IndicatorSource};

#[test]
fn every_value_of_every_type_reads_as_the_web_reads_it() {
    let fixture = fixture(TYPES);
    let cases = fixture["cases"].as_array().expect("cases");
    let candles = records(&json!({"shape": "trend"}));
    let mut failures = Failures::default();
    for case in cases {
        let params = IndicatorParams::from_pairs(pairs(&case["params"]));
        let got = compute_indicator_as_web(&IndicatorRequest {
            indicator_name: case["name"].as_str().expect("name"),
            params: &params,
            source: IndicatorSource::Candles {
                records: &candles,
                lookback_bars: 100,
            },
            tail_bars: 5,
            offset_bars: 0,
        });
        let problem = match (&got, case.get("result"), case.get("error")) {
            (Ok(out), Some(want), _) => {
                let v = out.to_value();
                let field = |k: &str| v.get(k).cloned().unwrap_or(Value::Null);
                let seen = json!({
                    "status": field("status"),
                    "latest": field("latest"),
                    "message": field("message"),
                });
                diff(&seen, want)
            }
            (Err(e), _, Some(want)) => error_diff(e, want),
            (Ok(out), None, Some(want)) => Some(format!(
                "computed {} but the web raised {want}",
                out.to_value()["latest"]
            )),
            (Err(e), Some(_), None) => Some(format!("raised {e:?} but the web computed")),
            _ => Some("malformed case".into()),
        };
        if let Some(problem) = problem {
            failures.push(format!("{} {}", case["name"], case["params"]), problem);
        }
    }
    failures.check(cases.len());
}
