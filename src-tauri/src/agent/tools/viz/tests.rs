//! The viz toolkit end to end: the registry entry and the web's specs,
//! then each chart through the in-process `/api/v1` and the `/tools`
//! services to the mock broker, the frame it leaves and the line the model
//! reads (`test_agent_tool_arguments.py`'s viz cases, Q-465, and the
//! wordings of `tools/viz.py`).

use super::*;
use crate::agent::contract::frames::Frame;
use crate::agent::contract::tool::Surface;
use crate::agent::contract::viz::VizSink;
use crate::agent::tools::testkit::Kit;
use crate::agent::tools::{spec_contract, TOOLKITS};
use crate::brokers::mock::MockCall;
use crate::brokers::types::{Candle, Quote};

fn quote(symbol: &str, exchange: &str, ltp: f64, oi: i64) -> Quote {
    Quote {
        symbol: symbol.into(),
        exchange: exchange.into(),
        ltp,
        open: ltp,
        high: ltp,
        low: ltp,
        close: ltp,
        volume: 1000,
        oi,
        ..Default::default()
    }
}

/// NIFTY at 24,510 on its index feed, and every 27OCT26 strike quoting
/// with open interest that peaks at 24,700 for calls and 24,300 for puts.
fn quoted_chain(kit: &Kit) {
    kit.mock.set_quote(quote("NIFTY", "NSE_INDEX", 24510.0, 0));
    for strike in (24000_i64..=25000).step_by(100) {
        let k = strike as f64;
        let call = (24510.0 - k).max(0.0) + 40.0;
        let put = (k - 24510.0).max(0.0) + 40.0;
        let call_oi = 100_000 - (strike - 24700).abs() * 100;
        let put_oi = 100_000 - (strike - 24300).abs() * 100;
        kit.mock.set_quote(quote(
            &format!("NIFTY27OCT26{}CE", strike),
            "NFO",
            call,
            call_oi,
        ));
        kit.mock.set_quote(quote(
            &format!("NIFTY27OCT26{}PE", strike),
            "NFO",
            put,
            put_oi,
        ));
    }
}

fn candles(n: i64, step: i64) -> Vec<Candle> {
    (0..n)
        .map(|i| Candle {
            timestamp: 1_759_000_000 + i * step,
            open: 100.0 + i as f64,
            high: 105.0 + i as f64,
            low: 95.0 + i as f64,
            close: 101.0 + i as f64,
            volume: 1000,
            oi: 0,
        })
        .collect()
}

/// The one frame a call left, as (kind, spec, title, source).
fn only_viz(ctx: &ToolCtx) -> (String, Map<String, Value>, String, String) {
    let frames = ctx.viz.drain();
    assert_eq!(frames.len(), 1, "{:?}", frames);
    match frames.into_iter().next() {
        Some(Frame::Viz {
            kind,
            spec,
            title,
            source,
        }) => (kind, spec, title, source),
        other => panic!("not a viz frame: {:?}", other),
    }
}

/// T-218: every surface, no capability, order 45, the web's four tools.
#[tokio::test]
async fn the_toolkit_is_registered_as_the_web_lists_it() {
    let group = TOOLKITS
        .iter()
        .find(|g| g.key == "viz")
        .expect("viz is registered");
    assert_eq!(group.surfaces, Surfaces::ALL);
    assert!(group.requires.is_empty());
    assert_eq!(group.order, 45);
    let kit = Kit::new();
    let chat = kit.ctx(Surface::Chat);
    let names: Vec<&str> = (GROUP.build)(&chat).iter().map(|t| t.spec().name).collect();
    assert_eq!(
        names,
        [
            "plot_price_chart",
            "plot_open_interest",
            "plot_gamma_exposure",
            "plot_volatility_surface"
        ]
    );
    spec_contract::assert_toolkit(&GROUP, &chat);
}

/// T-219..T-221: candles from the history service become a `candles` frame
/// with the web's spec, and the model reads one line.
#[tokio::test]
async fn a_price_chart_leaves_its_bars_on_the_sink_and_one_line_for_the_model() {
    let kit = Kit::new();
    *kit.mock.history.lock() = Some(Ok(candles(30, 86_400)));
    let ctx = kit.ctx(Surface::Chat);
    let out = kit
        .call_in(
            &GROUP,
            &ctx,
            "plot_price_chart",
            json!({"symbol": "infy", "exchange": "NSE", "indicators": "ema:20, rsi:14"}),
        )
        .await;
    assert!(out.is_ok(), "{:?}", out);
    let text = out.text();
    assert!(
        text.starts_with("<tool_result tool=\"plot_price_chart\" symbol=\"INFY\" exchange=\"NSE\""),
        "{}",
        text
    );
    assert!(text.contains(
        "Drew a candlestick chart of INFY on NSE at D with ema(20), rsi(14), 30 bars from "
    ));
    assert!(text.contains(". Open 100, close 130, high 134, low 95, change "));
    assert!(text
        .contains("The operator can see it, so describe what it shows rather than listing bars."));
    assert!(!text.contains("\"bars\""));
    let (kind, spec, title, source) = only_viz(&ctx);
    assert_eq!(
        (kind.as_str(), title.as_str(), source.as_str()),
        ("candles", "INFY NSE D", "history_service")
    );
    let mut keys: Vec<&str> = spec.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "bar_count",
            "bars",
            "chart_type",
            "end_date",
            "exchange",
            "indicators",
            "interval",
            "source",
            "start_date",
            "summary",
            "symbol",
            "timezone"
        ]
    );
    assert_eq!(spec["bar_count"], 30);
    assert_eq!(spec["timezone"], "Asia/Kolkata");
    assert_eq!(
        spec["indicators"][1],
        json!({"id": "rsi", "inputs": {"length": 14}})
    );
    assert_eq!(spec["bars"][0]["time"], 1_759_000_000);
    assert_eq!(spec["summary"]["last_close"], 130.0);
}

/// L-056: at most 1,500 bars, the newest, with a notice; the summary still
/// covers the whole range.
#[tokio::test]
async fn a_long_range_keeps_the_newest_bars_and_says_so() {
    let kit = Kit::new();
    *kit.mock.history.lock() = Some(Ok(candles(1600, 60)));
    let ctx = kit.ctx(Surface::Chat);
    let out = kit
        .call_in(
            &GROUP,
            &ctx,
            "plot_price_chart",
            json!({"symbol": "INFY", "exchange": "NSE", "interval": "1m"}),
        )
        .await;
    let notice = "100 of 1600 bars were older than the 1500 the chart carries, so the chart starts later than the range asked for. The summary covers the whole range.";
    assert!(
        out.text().contains(&format!(" Note: {}", notice)),
        "{}",
        out.text()
    );
    let (_, spec, _, _) = only_viz(&ctx);
    assert_eq!(spec["bar_count"], 1500);
    assert_eq!(spec["bars"][0]["time"], 1_759_000_000 + 100 * 60);
    assert_eq!(spec["notices"], json!([notice]));
    assert_eq!(spec["summary"]["first_open"], 100.0);
}

#[tokio::test]
async fn no_candles_draw_nothing_and_say_why() {
    let kit = Kit::new();
    *kit.mock.history.lock() = Some(Ok(Vec::new()));
    let ctx = kit.ctx(Surface::Chat);
    let out = kit
        .call_in(&GROUP, &ctx, "plot_price_chart", json!({"symbol": "INFY", "exchange": "NSE", "start_date": "2026-10-01", "end_date": "2026-10-09"}))
        .await;
    assert!(out.text().contains(
        "No candles came back for INFY on NSE at D between 2026-10-01 and 2026-10-09, so nothing was drawn. Check the dates against the trading calendar and confirm the interval is one this broker supports."
    ), "{}", out.text());
    assert!(ctx.viz.drain().is_empty());
}

/// T-221, T-235: a surface with no sink is told so, never that a chart is
/// on screen.
#[tokio::test]
async fn a_run_with_no_sink_says_the_chart_could_not_be_delivered() {
    let kit = Kit::new();
    *kit.mock.history.lock() = Some(Ok(candles(5, 86_400)));
    let mut ctx = kit.ctx(Surface::Chart);
    ctx.viz = VizSink::none();
    let out = kit
        .call_in(
            &GROUP,
            &ctx,
            "plot_price_chart",
            json!({"symbol": "INFY", "exchange": "NSE"}),
        )
        .await;
    assert!(out.text().contains(
        "The chart could not be delivered to this surface, so nothing was drawn. Answer in prose and do not tell the operator that a chart is on screen."
    ), "{}", out.text());
    assert!(!out.text().contains("Drew"));
}

#[tokio::test]
async fn a_shape_or_an_indicator_it_cannot_draw_is_refused_before_any_fetch() {
    let kit = Kit::new();
    let ctx = kit.ctx(Surface::Chat);
    let out = kit
        .call_in(
            &GROUP,
            &ctx,
            "plot_price_chart",
            json!({"symbol": "INFY", "exchange": "NSE", "chart_type": "renko"}),
        )
        .await;
    assert!(
        out.text()
            .contains("'renko' is not a chart shape this terminal draws"),
        "{}",
        out.text()
    );
    let out = kit
        .call_in(
            &GROUP,
            &ctx,
            "plot_price_chart",
            json!({"symbol": "INFY", "exchange": "NSE", "indicators": ["bolinger"]}),
        )
        .await;
    assert!(
        out.text()
            .contains("The closest ids the chart has are: bollinger."),
        "{}",
        out.text()
    );
    assert!(!kit
        .mock
        .calls()
        .iter()
        .any(|c| matches!(c, MockCall::History(_))));
    assert!(ctx.viz.drain().is_empty());
}

/// Q-465: NIFTY named on NSE is quoted on NSE_INDEX by each option chart,
/// and the answer is labelled as data.
#[tokio::test]
async fn nifty_on_nse_reaches_the_service_on_nse_index() {
    for tool in [
        "plot_open_interest",
        "plot_gamma_exposure",
        "plot_volatility_surface",
    ] {
        let kit = Kit::new();
        quoted_chain(&kit);
        let ctx = kit.ctx(Surface::Chat);
        let args = match tool {
            "plot_volatility_surface" => {
                json!({"underlying": "NIFTY", "exchange": "NSE", "expiry_dates": ["27OCT26"]})
            }
            _ => json!({"underlying": "NIFTY", "exchange": "NSE", "expiry_date": "27OCT26"}),
        };
        let out = kit.call_in(&GROUP, &ctx, tool, args).await;
        assert!(
            out.text().contains("<tool_result"),
            "{}: {}",
            tool,
            out.text()
        );
        let quoted_index = kit.mock.calls().iter().any(|c| {
            let keys = match c {
                MockCall::Quote(k) | MockCall::Depth(k) => vec![k.clone()],
                MockCall::MultiQuotes(ks) => ks.clone(),
                _ => Vec::new(),
            };
            keys.iter()
                .any(|k| format!("{:?}", k).contains("NSE_INDEX"))
        });
        assert!(quoted_index, "{}: {:?}", tool, kit.mock.calls());
        assert!(
            out.text().contains("Note: "),
            "{}: the index move is noticed: {}",
            tool,
            out.text()
        );
        let (kind, _, _, _) = only_viz(&ctx);
        assert_eq!(kind, "plotly");
    }
}

/// T-227, V-043: open interest by strike, calls green and puts red, ATM
/// and spot marked, and the peaks, totals and PCR in one line.
#[tokio::test]
async fn open_interest_is_two_bar_series_and_one_line() {
    let kit = Kit::new();
    quoted_chain(&kit);
    let ctx = kit.ctx(Surface::Chat);
    let out = kit
        .call_in(&GROUP, &ctx, "plot_open_interest", json!({"underlying": "NIFTY", "exchange": "NSE_INDEX", "expiry_date": "27OCT26", "strike_count": 5}))
        .await;
    let text = out.text();
    assert!(
        text.starts_with(
            "<tool_result tool=\"plot_open_interest\" underlying=\"NIFTY\" expiry=\"27OCT26\""
        ),
        "{}",
        text
    );
    assert!(text.contains("Drew open interest by strike for NIFTY 27OCT26: 11 strikes, ATM 24,500, spot 24,510. Call OI peaks at 24,700 and put OI at 24,300;"), "{}", text);
    let (kind, spec, title, source) = only_viz(&ctx);
    assert_eq!(
        (kind.as_str(), title.as_str(), source.as_str()),
        (
            "plotly",
            "NIFTY 27OCT26 open interest by strike",
            "option_chain_service"
        )
    );
    assert_eq!(spec["engine"], "2d");
    assert_eq!(spec["data"][0]["name"], "Call OI");
    assert_eq!(spec["data"][0]["marker"], json!({"color": "#22c55e"}));
    assert_eq!(spec["data"][1]["name"], "Put OI");
    assert_eq!(spec["data"][1]["marker"], json!({"color": "#ef4444"}));
    assert_eq!(spec["data"][0]["x"].as_array().map(Vec::len), Some(11));
    assert_eq!(spec["layout"]["barmode"], "group");
    assert_eq!(spec["layout"]["annotations"][0]["text"], "ATM");
    assert_eq!(spec["layout"]["annotations"][1]["text"], "Spot");
    assert_eq!(
        spec["config"],
        json!({"displayModeBar": false, "responsive": true})
    );
}

#[tokio::test]
async fn a_strike_count_that_is_not_a_number_is_refused() {
    let kit = Kit::new();
    let ctx = kit.ctx(Surface::Chat);
    let out = kit
        .call_in(&GROUP, &ctx, "plot_open_interest", json!({"underlying": "NIFTY", "exchange": "NSE_INDEX", "expiry_date": "27OCT26", "strike_count": "abc"}))
        .await;
    assert!(
        out.text()
            .contains("was not run because its arguments were refused")
            && out.text().contains("strike_count"),
        "{}",
        out.text()
    );
    assert!(ctx.viz.drain().is_empty());
}
