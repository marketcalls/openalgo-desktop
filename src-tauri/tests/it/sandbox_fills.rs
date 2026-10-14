//! Table-driven fill matrix: every price type x side x path (immediate at
//! placement, tick, polling fallback), with the trigger met or not and the
//! limit met or not. Expected outcomes follow the web's
//! `sandbox/order_manager.py` (placement) and `sandbox/execution_engine.py`
//! (`_process_order`, `_process_trigger_pending_order`).

use crate::sandbox_support;

use openalgo_desktop_lib::sandbox::{Quote, Tick};
use rust_decimal::Decimal;
use sandbox_support::*;

#[derive(Debug, Clone, Copy)]
enum Path {
    Tick,
    Poll,
}

struct Case {
    name: &'static str,
    action: &'static str,
    pt: &'static str,
    price: Option<&'static str>,
    trigger: Option<&'static str>,
    /// LTP when the order is placed.
    placement_ltp: &'static str,
    /// Status right after placement.
    after_place: &'static str,
    /// Average fill price after placement (if it filled).
    placed_fill: Option<&'static str>,
    /// LTPs delivered afterwards, with the status expected after each.
    steps: &'static [(&'static str, &'static str, Option<&'static str>)],
}

const CASES: &[Case] = &[
    Case {
        name: "market buy fills at ltp when the quote has no depth",
        action: "BUY",
        pt: "MARKET",
        price: None,
        trigger: None,
        placement_ltp: "100",
        after_place: "complete",
        placed_fill: Some("100"),
        steps: &[],
    },
    Case {
        name: "market sell fills at ltp when the quote has no depth",
        action: "SELL",
        pt: "MARKET",
        price: None,
        trigger: None,
        placement_ltp: "100",
        after_place: "complete",
        placed_fill: Some("100"),
        steps: &[],
    },
    Case {
        name: "marketable limit buy fills at ltp, not the limit",
        action: "BUY",
        pt: "LIMIT",
        price: Some("105"),
        trigger: None,
        placement_ltp: "100",
        after_place: "complete",
        placed_fill: Some("100"),
        steps: &[],
    },
    Case {
        name: "marketable limit sell fills at ltp, not the limit",
        action: "SELL",
        pt: "LIMIT",
        price: Some("95"),
        trigger: None,
        placement_ltp: "100",
        after_place: "complete",
        placed_fill: Some("100"),
        steps: &[],
    },
    Case {
        name: "resting limit buy fills at its limit when the price falls through",
        action: "BUY",
        pt: "LIMIT",
        price: Some("95"),
        trigger: None,
        placement_ltp: "100",
        after_place: "open",
        placed_fill: None,
        steps: &[("96", "open", None), ("95", "complete", Some("95"))],
    },
    Case {
        name: "resting limit buy fills at its limit even when the tick is lower",
        action: "BUY",
        pt: "LIMIT",
        price: Some("95"),
        trigger: None,
        placement_ltp: "100",
        after_place: "open",
        placed_fill: None,
        steps: &[("90", "complete", Some("95"))],
    },
    Case {
        name: "resting limit sell fills at its limit when the price rises through",
        action: "SELL",
        pt: "LIMIT",
        price: Some("105"),
        trigger: None,
        placement_ltp: "100",
        after_place: "open",
        placed_fill: None,
        steps: &[("104.95", "open", None), ("106", "complete", Some("105"))],
    },
    Case {
        name: "sl buy waits in trigger pending, then fills at ltp inside the limit",
        action: "BUY",
        pt: "SL",
        price: Some("106"),
        trigger: Some("105"),
        placement_ltp: "100",
        after_place: "trigger pending",
        placed_fill: None,
        steps: &[
            ("104", "trigger pending", None),
            ("105.5", "complete", Some("105.5")),
        ],
    },
    Case {
        name: "sl buy triggered beyond its limit rests open, then fills inside it",
        action: "BUY",
        pt: "SL",
        price: Some("106"),
        trigger: Some("105"),
        placement_ltp: "100",
        after_place: "trigger pending",
        placed_fill: None,
        steps: &[
            ("107", "open", None),
            ("108", "open", None),
            ("105.5", "complete", Some("105.5")),
        ],
    },
    Case {
        name: "sl buy with trigger and limit met at placement fills at ltp",
        action: "BUY",
        pt: "SL",
        price: Some("106"),
        trigger: Some("99"),
        placement_ltp: "100",
        after_place: "complete",
        placed_fill: Some("100"),
        steps: &[],
    },
    Case {
        name: "sl buy with trigger met but limit not at placement starts open",
        action: "BUY",
        pt: "SL",
        price: Some("98"),
        trigger: Some("97"),
        placement_ltp: "100",
        after_place: "open",
        placed_fill: None,
        steps: &[("97.5", "complete", Some("97.5"))],
    },
    Case {
        name: "sl sell waits, then fills at ltp inside the limit",
        action: "SELL",
        pt: "SL",
        price: Some("94"),
        trigger: Some("95"),
        placement_ltp: "100",
        after_place: "trigger pending",
        placed_fill: None,
        steps: &[
            ("96", "trigger pending", None),
            ("94.5", "complete", Some("94.5")),
        ],
    },
    Case {
        name: "sl sell triggered below its limit rests open",
        action: "SELL",
        pt: "SL",
        price: Some("94"),
        trigger: Some("95"),
        placement_ltp: "100",
        after_place: "trigger pending",
        placed_fill: None,
        steps: &[("93", "open", None), ("94", "complete", Some("94"))],
    },
    Case {
        name: "sl-m buy fills at ltp once the trigger is touched",
        action: "BUY",
        pt: "SL-M",
        price: None,
        trigger: Some("105"),
        placement_ltp: "100",
        after_place: "trigger pending",
        placed_fill: None,
        steps: &[
            ("104.95", "trigger pending", None),
            ("105", "complete", Some("105")),
        ],
    },
    Case {
        name: "sl-m buy with trigger met at placement fills at ltp",
        action: "BUY",
        pt: "SL-M",
        price: None,
        trigger: Some("99"),
        placement_ltp: "100",
        after_place: "complete",
        placed_fill: Some("100"),
        steps: &[],
    },
    Case {
        name: "sl-m sell fills at ltp once the trigger is touched",
        action: "SELL",
        pt: "SL-M",
        price: None,
        trigger: Some("95"),
        placement_ltp: "100",
        after_place: "trigger pending",
        placed_fill: None,
        steps: &[
            ("95.05", "trigger pending", None),
            ("94", "complete", Some("94")),
        ],
    },
    Case {
        name: "sl-m sell with trigger met at placement fills at ltp",
        action: "SELL",
        pt: "SL-M",
        price: None,
        trigger: Some("101"),
        placement_ltp: "100",
        after_place: "complete",
        placed_fill: Some("100"),
        steps: &[],
    },
];

async fn run_case(c: &Case, path: Path) {
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", c.placement_ltp);
    let mut r = req("SBIN", "NSE", c.action, 10, c.pt, "MIS");
    if let Some(p) = c.price {
        r = limit(r, p);
    }
    if let Some(t) = c.trigger {
        r = trigger(r, t);
    }
    let id = place(&env, r).await;
    assert_eq!(
        env.status(&id).await,
        c.after_place,
        "{} ({:?}): after placement",
        c.name,
        path
    );
    if let Some(f) = c.placed_fill {
        let o = env.sb.order_row(&id).await.unwrap().unwrap();
        assert_eq!(
            o.average_price,
            Some(d(f)),
            "{}: placement fill price",
            c.name
        );
    }
    for (ltp, status, fill) in c.steps {
        match path {
            Path::Tick => {
                env.sb
                    .on_tick(Tick::new("SBIN", "NSE", d(ltp)))
                    .await
                    .unwrap();
            }
            Path::Poll => {
                env.ltp("SBIN", "NSE", ltp);
                env.sb.poll_once().await.unwrap();
            }
        }
        assert_eq!(
            &env.status(&id).await,
            status,
            "{} ({:?}) at {}",
            c.name,
            path,
            ltp
        );
        if let Some(f) = fill {
            let o = env.sb.order_row(&id).await.unwrap().unwrap();
            assert_eq!(
                o.average_price,
                Some(d(f)),
                "{} ({:?}): fill price",
                c.name,
                path
            );
            assert_eq!(o.filled_quantity, 10);
            assert_eq!(o.pending_quantity, 0);
        }
    }
    env.assert_margin_consistent().await;
    env.shutdown().await;
}

#[tokio::test]
async fn fill_matrix_tick_path() {
    for c in CASES {
        run_case(c, Path::Tick).await;
    }
}

#[tokio::test]
async fn fill_matrix_polling_path() {
    for c in CASES {
        run_case(c, Path::Poll).await;
    }
}

#[tokio::test]
async fn market_order_fills_at_the_far_side_of_the_quote() {
    let env = Env::at("2026-10-05 10:00:00");
    env.quote(
        "SBIN",
        "NSE",
        Quote {
            ltp: d("100"),
            bid: d("99.95"),
            ask: d("100.05"),
            ..Quote::default()
        },
    );
    let b = place(&env, req("SBIN", "NSE", "BUY", 1, "MARKET", "MIS")).await;
    let s = place(&env, req("SBIN", "NSE", "SELL", 1, "MARKET", "MIS")).await;
    let ob = env.sb.order_row(&b).await.unwrap().unwrap();
    let os = env.sb.order_row(&s).await.unwrap().unwrap();
    assert_eq!(ob.average_price, Some(d("100.05")), "BUY at the ask");
    assert_eq!(os.average_price, Some(d("99.95")), "SELL at the bid");
    assert_eq!(
        ob.price,
        Some(d("100")),
        "the MARKET row stores the LTP used for margin"
    );
    env.shutdown().await;
}

#[tokio::test]
async fn a_market_order_without_a_quote_rests_on_the_last_position_price_and_fills_on_the_next_tick(
) {
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 1, "MARKET", "MIS")).await;
    env.quotes.remove("SBIN", "NSE");
    let id = place(&env, req("SBIN", "NSE", "BUY", 1, "MARKET", "MIS")).await;
    assert_eq!(env.status(&id).await, "open");
    env.sb
        .on_tick(Tick::new("SBIN", "NSE", d("101")))
        .await
        .unwrap();
    assert_eq!(env.status(&id).await, "complete");
    assert_eq!(env.qty("SBIN", "NSE", "MIS").await, 2);
    env.shutdown().await;
}

#[tokio::test]
async fn a_market_order_with_no_price_anywhere_is_refused() {
    let env = Env::at("2026-10-05 10:00:00");
    let e = env
        .sb
        .place_order(req("SBIN", "NSE", "BUY", 1, "MARKET", "MIS"))
        .await
        .unwrap_err();
    assert_eq!(e.http_status, 400);
    assert!(e
        .message
        .starts_with("Cannot place MARKET order for SBIN - unable to fetch current price"));
    env.shutdown().await;
}

// Port of test/sandbox/test_stale_quote_guard.py::test_both_fill_paths_consult_the_guard
#[tokio::test]
async fn test_both_fill_paths_consult_the_guard() {
    let env = Env::at("2026-10-05 10:00:00");
    let stale = Quote {
        ltp: d("1047.60"),
        high: d("1345"),
        low: d("1262"),
        ..Quote::default()
    };
    env.quote("RELIANCE", "NSE", stale);
    // Placement path: a MARKET order is deferred, not filled at the stale LTP.
    let m = place(&env, req("RELIANCE", "NSE", "BUY", 1, "MARKET", "MIS")).await;
    assert_eq!(env.status(&m).await, "open");
    // Placement path for a marketable LIMIT too.
    let l = place(
        &env,
        limit(req("RELIANCE", "NSE", "BUY", 1, "LIMIT", "MIS"), "1100"),
    )
    .await;
    assert_eq!(env.status(&l).await, "open");
    // Polling path with the same stale quote: still nothing.
    env.sb.poll_once().await.unwrap();
    assert_eq!(env.status(&m).await, "open");
    // A coherent quote fills both.
    env.quote(
        "RELIANCE",
        "NSE",
        Quote {
            ltp: d("1296"),
            high: d("1345"),
            low: d("1262"),
            ..Quote::default()
        },
    );
    env.sb.poll_once().await.unwrap();
    assert_eq!(env.status(&m).await, "complete");
    assert_eq!(
        env.sb.order_row(&m).await.unwrap().unwrap().average_price,
        Some(d("1296"))
    );
    assert_eq!(
        env.status(&l).await,
        "open",
        "a 1100 limit does not fill at 1296"
    );
    env.shutdown().await;
}

#[tokio::test]
async fn every_transition_publishes_an_order_update() {
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "100");
    let sl = place(
        &env,
        limit(
            trigger(req("SBIN", "NSE", "BUY", 1, "SL", "MIS"), "105"),
            "106",
        ),
    )
    .await;
    env.sb
        .on_tick(Tick::new("SBIN", "NSE", d("107")))
        .await
        .unwrap();
    env.sb
        .on_tick(Tick::new("SBIN", "NSE", d("105.5")))
        .await
        .unwrap();
    env.rec.wait_for("sandbox.order_filled", 1).await;
    env.rec.wait_for("order.update", 3).await;
    assert_eq!(
        env.rec.statuses(&sl),
        vec!["trigger pending", "open", "complete"]
    );
    let c = place(
        &env,
        limit(req("SBIN", "NSE", "BUY", 1, "LIMIT", "MIS"), "90"),
    )
    .await;
    env.sb.cancel_order(&c).await.unwrap();
    env.rec.wait_for("order.update", 5).await;
    assert_eq!(env.rec.statuses(&c), vec!["open", "cancelled"]);
    env.shutdown().await;
}

#[tokio::test]
async fn a_modify_after_the_decision_is_not_filled_at_the_old_terms() {
    // Port of test_gthread_sandbox_fills::test_a_modify_after_the_fill_was_decided_is_not_filled_at_the_old_terms
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "100");
    let id = place(
        &env,
        limit(req("SBIN", "NSE", "BUY", 1, "LIMIT", "MIS"), "95"),
    )
    .await;
    let decided = env.sb.order_row(&id).await.unwrap().unwrap();
    env.sb
        .modify_order(
            &id,
            openalgo_desktop_lib::sandbox::ModifyRequest {
                price: Some(d("90")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // A fill decided against the old terms (limit 95 at a 94 tick) must not land.
    let filled = openalgo_desktop_lib::sandbox::execution::decide(&decided, &Quote::ltp(d("94")));
    assert_eq!(
        filled,
        openalgo_desktop_lib::sandbox::execution::Decision::Fill(d("95"))
    );
    env.sb
        .on_tick(Tick::new("SBIN", "NSE", d("94")))
        .await
        .unwrap();
    assert_eq!(
        env.status(&id).await,
        "open",
        "the modified 90 limit does not fill at 94"
    );
    assert_eq!(decided.price, Some(Decimal::from(95)));
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// A cancel releases exactly the margin the order reserved (openalgo#2207)
// ---------------------------------------------------------------------------

/// Web test/test_gthread_sandbox_fills.py::test_cancel_closing_mis_order_does_not_release_unreserved_margin
/// and ::test_cancel_pre_marker_zero_margin_closing_order_preserves_position_margin.
///
/// A resting order that closes an open position reserves nothing, so its
/// cancel releases nothing: the position's margin stays blocked. The web
/// recalculated a margin for any order recorded with zero and released it,
/// freeing margin the position still held. The desktop records the exact
/// reservation on every order (no legacy rows without one), so the web's
/// NULL-versus-zero marker has no counterpart and both web cases are this
/// one row.
#[tokio::test]
async fn cancel_of_a_closing_mis_order_releases_no_margin() {
    let env = Env::at("2026-10-05 14:00:00");
    env.ltp("RELIANCE", "NSE", "2600");
    // Open a long MIS position, then leave a SELL limit resting to close it.
    place(&env, req("RELIANCE", "NSE", "BUY", 10, "MARKET", "MIS")).await;
    assert_eq!(env.qty("RELIANCE", "NSE", "MIS").await, 10);
    let closing = place(
        &env,
        limit(req("RELIANCE", "NSE", "SELL", 10, "LIMIT", "MIS"), "2700"),
    )
    .await;
    let row = env.sb.order_row(&closing).await.unwrap().unwrap();
    assert_eq!(row.order_status.as_str(), "open");
    assert_eq!(
        row.margin_blocked,
        Decimal::ZERO,
        "a closing order reserves nothing"
    );
    let used_before_cancel = env.used().await;
    assert_eq!(
        used_before_cancel,
        d("5200"),
        "the position's MIS margin, 26000 / 5"
    );

    env.sb.cancel_order(&closing).await.unwrap();

    assert_eq!(env.status(&closing).await, "cancelled");
    assert_eq!(
        env.used().await,
        used_before_cancel,
        "the position's margin stays blocked"
    );
    env.assert_margin_consistent().await;

    // An order that closes and then reverses reserves only the reversal,
    // and a cancel gives back exactly that.
    let reversing = place(
        &env,
        limit(req("RELIANCE", "NSE", "SELL", 15, "LIMIT", "MIS"), "2700"),
    )
    .await;
    let row = env.sb.order_row(&reversing).await.unwrap().unwrap();
    assert_eq!(row.margin_blocked, d("2700"), "5 short at 2700 / 5");
    assert_eq!(env.used().await, d("7900"));
    env.sb.cancel_order(&reversing).await.unwrap();
    assert_eq!(env.used().await, used_before_cancel);
    env.assert_margin_consistent().await;
    env.shutdown().await;
}

/// Web test/test_gthread_sandbox_fills.py::test_cancel_legacy_zero_margin_field_recalculates_opening_order_reservation.
///
/// The web case is a legacy row whose margin column was migrated in as zero
/// while the order held margin; the web recalculates and releases it. The
/// desktop has no such rows, so the port checks the outcome the web test
/// asserts: cancelling a resting opening order gives back everything it
/// reserved.
#[tokio::test]
async fn cancel_of_an_opening_mis_order_releases_its_reservation() {
    let env = Env::at("2026-10-05 14:00:00");
    env.ltp("RELIANCE", "NSE", "2600");
    let id = place(
        &env,
        limit(req("RELIANCE", "NSE", "BUY", 10, "LIMIT", "MIS"), "2500"),
    )
    .await;
    assert_eq!(env.status(&id).await, "open");
    assert_eq!(env.used().await, d("5000"), "25000 / 5 reserved");

    env.sb.cancel_order(&id).await.unwrap();

    assert_eq!(env.status(&id).await, "cancelled");
    assert_eq!(env.used().await, d("0"));
    assert_eq!(env.available().await, d("10000000"));
    env.assert_margin_consistent().await;
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// A fill is all or nothing: no order completes without its trade row
// ---------------------------------------------------------------------------

// SB-02: a trade insert that fails for any reason other than a duplicate
// trade id rolls the whole fill back, and the order is rejected with its
// margin returned.
#[tokio::test]
async fn a_fill_whose_trade_cannot_be_recorded_is_rejected_and_rolled_back() {
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "100");
    env.sb
        .db()
        .with_conn(|c| {
            c.execute_batch(
                "CREATE TRIGGER refuse_trades BEFORE INSERT ON sandbox_trades
                 BEGIN SELECT RAISE(ABORT, 'trade refused'); END;",
            )
        })
        .unwrap();
    let id = place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "MIS")).await;

    assert_eq!(env.status(&id).await, "rejected");
    let o = env.sb.order_row(&id).await.unwrap().unwrap();
    assert_eq!(o.filled_quantity, 0);
    assert_eq!(o.average_price, None);
    assert!(
        env.sb
            .position_row("SBIN", "NSE", "MIS")
            .await
            .unwrap()
            .is_none(),
        "no position from a fill that was not recorded"
    );
    assert_eq!(env.qty("SBIN", "NSE", "MIS").await, 0);
    assert_eq!(env.used().await, d("0"), "the order's margin is returned");
    assert_eq!(env.available().await, d("10000000"));
    assert!(env.sb.tradebook().await.unwrap().data.is_empty());
    env.rec.wait_for("order.update", 2).await;
    assert_eq!(env.rec.statuses(&id), vec!["open", "rejected"]);
    assert_eq!(
        env.rec.count("sandbox.order_filled"),
        0,
        "no fill event for a fill that rolled back"
    );
    env.assert_margin_consistent().await;
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// Closing a position counts what filled; margin follows the position
// ---------------------------------------------------------------------------

fn open_orders(
    book: &openalgo_desktop_lib::sandbox::replies::OrderbookReply,
) -> Vec<(String, String, i64)> {
    book.data
        .orders
        .iter()
        .filter(|o| o.order_status == "open" || o.order_status == "trigger pending")
        .map(|o| (o.orderid.clone(), o.action.clone(), o.quantity))
        .collect()
}

// SB-05: a close that cannot fill yet (no price) is reported as pending, not
// closed, and asking again returns the same pending close instead of placing
// a second exit that would reverse the position with no margin.
#[tokio::test]
async fn a_close_without_a_price_is_pending_and_repeat_closes_do_not_reverse() {
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "MIS")).await;
    assert_eq!(env.qty("SBIN", "NSE", "MIS").await, 10);

    env.quotes.remove("SBIN", "NSE");
    for attempt in 0..2 {
        let reply = env.sb.close_all_positions().await.unwrap();
        let v = serde_json::to_value(&reply).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec![
                "closed_positions",
                "failed_closures",
                "message",
                "mode",
                "status"
            ],
            "the web's close-all keys, exactly"
        );
        assert_eq!(v["status"], "success");
        assert_eq!(v["closed_positions"], 0, "attempt {attempt}: {v}");
        assert_eq!(v["failed_closures"], 0, "attempt {attempt}: {v}");
        assert!(
            v["message"]
                .as_str()
                .unwrap()
                .contains("1 close order waiting for a price"),
            "attempt {attempt}: {v}"
        );
    }
    let pending = open_orders(&env.sb.orderbook().await.unwrap());
    assert_eq!(pending.len(), 1, "one pending close: {pending:?}");
    assert_eq!((pending[0].1.as_str(), pending[0].2), ("SELL", 10));
    assert_eq!(env.qty("SBIN", "NSE", "MIS").await, 10);

    // Prices return: the one close fills and the position is flat.
    env.ltp("SBIN", "NSE", "101");
    env.sb.poll_once().await.unwrap();
    assert_eq!(env.status(&pending[0].0).await, "complete");
    assert_eq!(env.qty("SBIN", "NSE", "MIS").await, 0);
    assert_eq!(env.used().await, d("0"));
    env.assert_margin_consistent().await;
    env.shutdown().await;
}

// SB-05: a stop and a target each sized to the whole position both fill.
// The second one opens a new position, so it blocks that position's margin
// when it fills (the web left it at zero).
#[tokio::test]
async fn an_exit_that_fills_after_the_position_is_flat_blocks_its_own_margin() {
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "MIS")).await;
    assert_eq!(env.used().await, d("200"));
    let stop = place(
        &env,
        trigger(req("SBIN", "NSE", "SELL", 10, "SL-M", "MIS"), "95"),
    )
    .await;
    let target = place(
        &env,
        limit(req("SBIN", "NSE", "SELL", 10, "LIMIT", "MIS"), "105"),
    )
    .await;
    assert_eq!(
        env.used().await,
        d("200"),
        "exits block nothing while the position covers them"
    );

    env.sb
        .on_tick(Tick::new("SBIN", "NSE", d("105")))
        .await
        .unwrap();
    assert_eq!(env.status(&target).await, "complete");
    assert_eq!(env.qty("SBIN", "NSE", "MIS").await, 0);
    assert_eq!(env.used().await, d("0"));

    env.sb
        .on_tick(Tick::new("SBIN", "NSE", d("95")))
        .await
        .unwrap();
    assert_eq!(env.status(&stop).await, "complete");
    assert_eq!(env.qty("SBIN", "NSE", "MIS").await, -10);
    let p = env
        .sb
        .position_row("SBIN", "NSE", "MIS")
        .await
        .unwrap()
        .unwrap();
    // 10 x 95 / 5x MIS leverage.
    assert_eq!(p.margin_blocked, d("190"), "the new short holds its margin");
    assert_eq!(env.used().await, d("190"));
    env.assert_margin_consistent().await;
    let f = env.sb.funds_row().await.unwrap().unwrap();
    assert_eq!(
        f.available_balance + f.used_margin - f.realized_pnl,
        d("10000000")
    );
    env.shutdown().await;
}

// SB-05: when the account cannot carry the position an exit would open, the
// exit is rejected when it would fill, and nothing else moves.
#[tokio::test]
async fn an_exit_that_would_open_a_position_without_funds_is_rejected_at_fill() {
    let env = Env::at("2026-10-05 10:00:00");
    env.sb
        .update_config("starting_capital", "100000")
        .await
        .unwrap();
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 2000, "MARKET", "MIS")).await;
    let stop = place(
        &env,
        trigger(req("SBIN", "NSE", "SELL", 2000, "SL-M", "MIS"), "95"),
    )
    .await;
    let target = place(
        &env,
        limit(req("SBIN", "NSE", "SELL", 2000, "LIMIT", "MIS"), "105"),
    )
    .await;
    env.sb
        .on_tick(Tick::new("SBIN", "NSE", d("105")))
        .await
        .unwrap();
    assert_eq!(env.status(&target).await, "complete");
    assert_eq!(env.qty("SBIN", "NSE", "MIS").await, 0);
    // Use most of what is left: 410 x 900 / 5 = 73,800 of 1,10,000.
    env.ltp("INFY", "NSE", "1000");
    place(
        &env,
        limit(req("INFY", "NSE", "BUY", 410, "LIMIT", "MIS"), "900"),
    )
    .await;
    assert_eq!(env.used().await, d("73800"));
    let fills_before = env.rec.count("sandbox.order_filled");

    // The stop would open a 2000 short needing 38,000 with 36,200 free.
    env.sb
        .on_tick(Tick::new("SBIN", "NSE", d("95")))
        .await
        .unwrap();
    let o = env.sb.order_row(&stop).await.unwrap().unwrap();
    assert_eq!(o.order_status.as_str(), "rejected");
    assert!(
        o.rejection_reason
            .as_deref()
            .unwrap_or_default()
            .starts_with("Insufficient funds"),
        "{:?}",
        o.rejection_reason
    );
    assert_eq!(o.filled_quantity, 0);
    assert_eq!(env.qty("SBIN", "NSE", "MIS").await, 0);
    assert_eq!(env.used().await, d("73800"));
    env.assert_margin_consistent().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(env.rec.count("sandbox.order_filled"), fills_before);
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// An old price is not filled at
// ---------------------------------------------------------------------------

// SB-06: a tick received longer ago than the stale-quote limit (one that sat
// in a backlog) does not fill a resting order; the same price received now
// does.
#[tokio::test]
async fn a_tick_received_too_long_ago_is_not_filled_at() {
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "100");
    let id = place(
        &env,
        limit(req("SBIN", "NSE", "BUY", 10, "LIMIT", "MIS"), "95"),
    )
    .await;
    let old = Tick {
        received_at: std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(60))
            .expect("uptime"),
        ..Tick::new("SBIN", "NSE", d("94"))
    };
    assert_eq!(env.sb.on_tick(old).await.unwrap(), (0, 0));
    assert_eq!(env.status(&id).await, "open");
    assert_eq!(
        env.sb
            .on_tick(Tick::new("SBIN", "NSE", d("94")))
            .await
            .unwrap(),
        (1, 0)
    );
    assert_eq!(env.status(&id).await, "complete");
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// Crypto contracts: the multiplier scales margin and trade value as well
// ---------------------------------------------------------------------------

// SB-03: BTCUSD.P carries 0.001 BTC per contract. Margin and trade value use
// the contract's notional, the same as its P&L.
#[tokio::test]
async fn crypto_contract_value_applies_to_margin_trade_value_and_pnl() {
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("BTCUSD.P", "CRYPTO", "60000");
    let id = place(&env, req("BTCUSD.P", "CRYPTO", "BUY", 10, "MARKET", "NRML")).await;
    assert_eq!(env.status(&id).await, "complete");
    // 10 contracts x 0.001 x 60,000 = 600 notional, at 10x futures leverage.
    assert_eq!(env.used().await, d("60"));
    let p = env
        .sb
        .position_row("BTCUSD.P", "CRYPTO", "NRML")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.margin_blocked, d("60"));
    let tb = env.sb.tradebook().await.unwrap();
    assert_eq!(tb.data.len(), 1);
    assert_eq!(tb.data[0].trade_value, 600.0);

    // A 100-point move is 100 x 10 x 0.001 = 1.
    env.ltp("BTCUSD.P", "CRYPTO", "60100");
    let pb = env.sb.positionbook().await.unwrap();
    assert_eq!(pb.data[0].unrealized_pnl, 1.0);
    env.sb
        .close_position("BTCUSD.P", "CRYPTO", "NRML")
        .await
        .unwrap();
    assert_eq!(env.qty("BTCUSD.P", "CRYPTO", "NRML").await, 0);
    let f = env.sb.funds_row().await.unwrap().unwrap();
    assert_eq!(f.realized_pnl, d("1"));
    assert_eq!(f.used_margin, d("0"));
    assert_eq!(f.available_balance, d("10000001"));
    env.assert_margin_consistent().await;
    let values: Vec<f64> = env
        .sb
        .tradebook()
        .await
        .unwrap()
        .data
        .iter()
        .map(|t| t.trade_value)
        .collect();
    assert_eq!(values, vec![601.0, 600.0], "the close, then the open");
    env.shutdown().await;
}
