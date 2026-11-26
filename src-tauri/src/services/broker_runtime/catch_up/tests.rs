//! The order catch-up and the daily boundary of a single-stream session
//! (web 73599f37c, marketcalls/openalgo#2507, issue #2386), ported from
//! web `test/test_flattrade_issue_2386.py` where its cases apply. The
//! runtime's wiring, through a Flattrade session, is in
//! `brokers/flattrade/tests.rs` (`order_stream`).

use super::*;
use crate::clock::ManualClock;
use crate::error::AppError;
use chrono::TimeZone;
use chrono_tz::Asia::Kolkata;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};

/// An order book row (web `_order`): SBIN, 10 to buy at market, MIS.
fn order(id: &str, status: &str, filled: i32) -> Order {
    Order {
        order_id: id.into(),
        exchange_order_id: None,
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        side: "BUY".into(),
        quantity: 10,
        filled_quantity: filled,
        pending_quantity: 10 - filled,
        price: 0.0,
        trigger_price: 0.0,
        average_price: 0.0,
        order_type: "MARKET".into(),
        product: "MIS".into(),
        status: status.into(),
        validity: "DAY".into(),
        order_timestamp: String::new(),
        exchange_timestamp: None,
        rejection_reason: None,
        order_tag: None,
    }
}

/// An update the market socket pushed (web `OM_FRAME`, normalised).
fn pushed(id: &str, status: &str, filled: i64) -> OrderUpdate {
    OrderUpdate {
        orderid: id.into(),
        order_status: status.into(),
        filled_quantity: filled,
        ..Default::default()
    }
}

/// A read of `book` with nothing pushed while it was on its way.
fn read(c: &OrderCatchUp, book: &[Order]) -> Vec<OrderUpdate> {
    c.changed(book, c.issued())
}

fn ids(updates: &[OrderUpdate]) -> Vec<(&str, &str)> {
    updates
        .iter()
        .map(|u| (u.orderid.as_str(), u.order_status.as_str()))
        .collect()
}

/// Web `test_the_first_read_records_and_a_later_one_publishes_the_gap`.
/// The update carries the book's fields, the rejected order's reason
/// included: the web's book rows carry none (Flattrade's
/// `transform_order_data`), so its catch-up sends an empty one, lost data
/// the desktop does not copy.
#[test]
fn the_first_read_records_and_a_later_one_publishes_the_gap() {
    let c = OrderCatchUp::default();
    assert!(
        read(&c, &[order("1", "open", 0), order("2", "open", 0)]).is_empty(),
        "the first read must only record"
    );
    let mut rejected = order("3", "rejected", 0);
    rejected.rejection_reason = Some("RMS:Margin Exceeds".into());
    let gap = read(
        &c,
        &[order("1", "complete", 10), order("2", "open", 0), rejected],
    );
    assert_eq!(ids(&gap), vec![("1", "complete"), ("3", "rejected")]);
    assert_eq!((gap[0].filled_quantity, gap[0].pending_quantity), (10, 0));
    assert_eq!(gap[1].rejection_reason, "RMS:Margin Exceeds");
    assert!(
        read(
            &c,
            &[
                order("1", "complete", 10),
                order("2", "open", 0),
                order("3", "rejected", 0)
            ]
        )
        .is_empty(),
        "an unchanged book publishes nothing"
    );
}

/// Web `test_an_update_already_pushed_is_not_published_again`.
#[test]
fn an_update_already_pushed_is_not_published_again() {
    let c = OrderCatchUp::default();
    assert!(read(&c, &[order("26111700000001", "open", 0)]).is_empty());
    c.remember(&pushed("26111700000001", "complete", 10));
    assert!(read(&c, &[order("26111700000001", "complete", 10)]).is_empty());
}

/// Web `test_trigger_pending_and_open_are_the_same_state_to_the_catch_up`:
/// the push keeps `trigger pending`, the book folds it into `open`.
#[test]
fn trigger_pending_and_open_are_the_same_state_to_the_catch_up() {
    let c = OrderCatchUp::default();
    assert!(read(&c, &[]).is_empty());
    c.remember(&pushed("26111700000001", "trigger pending", 0));
    assert!(read(&c, &[order("26111700000001", "open", 0)]).is_empty());
}

/// A push that comes before the first read has finished is kept over
/// that read's row: the read was on its way when the order filled, and the
/// fill went out with the push, so the next read sends nothing more (the
/// web drops such a push, `_remember`, and sends the fill again).
#[test]
fn a_push_before_the_first_read_is_kept_over_it() {
    let c = OrderCatchUp::default();
    c.remember(&pushed("1", "complete", 10));
    assert!(read(&c, &[order("1", "open", 0)]).is_empty());
    assert!(read(&c, &[order("1", "complete", 10)]).is_empty());
    assert_eq!(
        ids(&read(&c, &[order("1", "cancelled", 10)])),
        vec![("1", "cancelled")]
    );
}

/// A push that comes while a later read is on its way is newer than that
/// read's row too: the row publishes nothing and leaves the pushed state,
/// and the next read settles the order. The web's `known.update` lets the
/// row overwrite it, so a fill pushed meanwhile went out again as open
/// after its complete, and as complete once more at the next read. A push
/// from before the read was sent is older than its rows, as on the web.
#[test]
fn a_push_while_any_read_is_on_its_way_is_kept_over_its_row() {
    let c = OrderCatchUp::default();
    assert!(read(&c, &[order("1", "open", 0), order("2", "open", 0)]).is_empty());
    c.remember(&pushed("2", "open", 0));
    let issued = c.issued();
    c.remember(&pushed("1", "complete", 10));
    let stale = [order("1", "open", 0), order("2", "cancelled", 0)];
    assert_eq!(
        ids(&c.changed(&stale, issued)),
        vec![("2", "cancelled")],
        "the row of 1 is older than its push; 2 was pushed before the read"
    );
    assert!(
        read(
            &c,
            &[order("1", "complete", 10), order("2", "cancelled", 0)]
        )
        .is_empty(),
        "the fill is not published again"
    );
}

fn connected() -> FeedStatus {
    FeedStatus::Connected {
        broker: "flattrade".into(),
    }
}

fn reconnecting() -> FeedStatus {
    FeedStatus::Reconnecting {
        broker: "flattrade".into(),
        attempt: 1,
        delay_ms: 5,
    }
}

/// Turns for the catch-up loop to see a status and finish its read.
async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

/// The reads `run` makes, each answered by the next of `books` (an
/// `Err` for `None`), what the socket pushes while one is on its way, and
/// what it publishes (order id and status).
struct Script {
    books: parking_lot::Mutex<VecDeque<Option<Vec<Order>>>>,
    reads: AtomicUsize,
    /// Read number (from 0) to the update pushed while it is on its way.
    pushed_during: parking_lot::Mutex<HashMap<usize, OrderUpdate>>,
    published: parking_lot::Mutex<Vec<(String, String)>>,
}

impl Script {
    fn new(books: Vec<Option<Vec<Order>>>) -> Self {
        Self {
            books: parking_lot::Mutex::new(books.into()),
            reads: AtomicUsize::new(0),
            pushed_during: parking_lot::Mutex::default(),
            published: parking_lot::Mutex::new(Vec::new()),
        }
    }

    /// The socket pushes `update` while read `n` (from 0) is on its way.
    fn pushing_during(self, n: usize, update: OrderUpdate) -> Self {
        self.pushed_during.lock().insert(n, update);
        self
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    fn published(&self) -> Vec<(String, String)> {
        self.published.lock().clone()
    }
}

fn update(id: &str, status: &str) -> (String, String) {
    (id.to_string(), status.to_string())
}

/// Run the catch-up loop over `script` on the feed status `rx` while
/// `drive` moves it (and drops its sender, which ends the loop), `current`
/// saying whether the session's day goes on.
async fn drive_run(
    script: &Script,
    current: bool,
    rx: watch::Receiver<FeedStatus>,
    drive: impl std::future::Future<Output = ()>,
) {
    let state = OrderCatchUp::default();
    let read = || {
        let n = script.reads.fetch_add(1, Ordering::SeqCst);
        if let Some(u) = script.pushed_during.lock().remove(&n) {
            state.remember(&u);
        }
        let next = script.books.lock().pop_front().flatten();
        async move {
            next.ok_or_else(|| AppError::Broker("Flattrade could not read the order book.".into()))
        }
    };
    let publish = |u: OrderUpdate| script.published.lock().push((u.orderid, u.order_status));
    tokio::join!(
        run(&state, rx, "flattrade", (read, || current, publish)),
        drive
    );
}

/// Web `test_ready_from_the_market_data_adapter_schedules_a_catch_up`:
/// each connect of the feed reads the order book once (the first only
/// records); a status on the way to it reads nothing.
#[tokio::test]
async fn each_connect_reads_the_order_book_once() {
    let script = Script::new(vec![
        Some(vec![order("1", "open", 0)]),
        Some(vec![order("1", "complete", 10)]),
    ]);
    let (send, rx) = watch::channel(FeedStatus::Disconnected);
    let drive = async {
        send.send_replace(FeedStatus::Connecting {
            broker: "flattrade".into(),
            attempt: 0,
        });
        settle().await;
        assert_eq!(script.reads(), 0, "no read before the login is accepted");
        send.send_replace(connected());
        settle().await;
        assert_eq!(script.reads(), 1);
        assert!(
            script.published.lock().is_empty(),
            "the first read only records"
        );
        send.send_replace(reconnecting());
        settle().await;
        assert_eq!(script.reads(), 1);
        send.send_replace(connected());
        settle().await;
        assert_eq!(script.reads(), 2);
        drop(send);
    };
    drive_run(&script, true, rx, drive).await;
    assert_eq!(script.published(), vec![update("1", "complete")]);
}

/// A read that fails publishes nothing and keeps the state it had: the
/// next connect's read publishes the change against it.
#[tokio::test]
async fn a_failed_read_publishes_nothing_and_keeps_the_state() {
    let script = Script::new(vec![
        Some(vec![order("1", "open", 0)]),
        None,
        Some(vec![order("1", "complete", 10)]),
    ]);
    let (send, rx) = watch::channel(FeedStatus::Disconnected);
    let drive = async {
        for _ in 0..3 {
            send.send_replace(connected());
            settle().await;
            send.send_replace(reconnecting());
            settle().await;
        }
        assert_eq!(script.reads(), 3);
        drop(send);
    };
    drive_run(&script, true, rx, drive).await;
    assert_eq!(script.published(), vec![update("1", "complete")]);
}

/// A fill the socket pushes while a reconnect's read is on its way, the
/// book answering from before it: nothing more is published, neither the
/// row's older state then nor the fill again at the next reconnect (web
/// `_catch_up`'s `known.update` published both).
#[tokio::test]
async fn a_fill_pushed_while_a_reconnect_reads_is_published_once() {
    let script = Script::new(vec![
        Some(vec![order("1", "open", 0)]),
        Some(vec![order("1", "open", 0)]),
        Some(vec![order("1", "complete", 10)]),
    ])
    .pushing_during(1, pushed("1", "complete", 10));
    let (send, rx) = watch::channel(FeedStatus::Disconnected);
    let drive = async {
        for _ in 0..3 {
            send.send_replace(connected());
            settle().await;
            send.send_replace(reconnecting());
            settle().await;
        }
        assert_eq!(script.reads(), 3);
        drop(send);
    };
    drive_run(&script, true, rx, drive).await;
    assert_eq!(script.published(), Vec::<(String, String)>::new());
}

/// Web `test_without_a_login_this_session_nothing_connects_or_reads`:
/// once the session's day has ended, a connect reads nothing.
#[tokio::test]
async fn after_the_daily_boundary_a_connect_reads_nothing() {
    let script = Script::new(vec![Some(vec![order("1", "open", 0)])]);
    let (send, rx) = watch::channel(FeedStatus::Disconnected);
    let drive = async {
        for _ in 0..3 {
            send.send_replace(connected());
            settle().await;
            send.send_replace(reconnecting());
            settle().await;
        }
        drop(send);
    };
    drive_run(&script, false, rx, drive).await;
    assert_eq!(script.reads(), 0);
    assert!(script.published.lock().is_empty());
}

fn ist(d: u32, h: u32, m: u32) -> DateTime<Utc> {
    Kolkata
        .with_ymd_and_hms(2026, 11, d, h, m, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc)
}

/// Web `test_the_session_check_measures_from_the_last_rollover` (03:00
/// IST): an evening login is current until 03:00, not after it; a login
/// after 03:00 is. The connect gate refuses exactly when it is not, with
/// what to do.
#[test]
fn the_session_day_measures_from_the_last_boundary() {
    for (now, login, current) in [
        (ist(18, 1, 30), ist(17, 21, 0), true),
        (ist(18, 5, 0), ist(17, 21, 0), false),
        (ist(18, 5, 0), ist(18, 4, 0), true),
    ] {
        let day = SessionDay::new(ManualClock::new(now), login, 3, 0);
        assert_eq!(day.current(), current, "now {} login {}", now, login);
        let refusal = day.gate("Flattrade")();
        match refusal {
            None => assert!(current),
            Some(why) => {
                assert!(!current);
                assert!(why.contains("Log in to Flattrade again"), "{}", why);
            }
        }
    }
}
