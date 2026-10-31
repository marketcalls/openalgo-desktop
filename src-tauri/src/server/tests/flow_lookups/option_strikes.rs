//! The option-strike route's day and its underlying split (parity C-11,
//! C-12; web `blueprints/flow.py` 1633-1700): the monthly expiry types
//! count from today in IST on any machine, and an underlying whose name
//! has digits is looked up whole.

use super::*;

/// The lookups' harness with the app clock at `now` from the start, so the
/// account, the session and the broker login all belong to that day.
fn at(now: DateTime<Utc>) -> L {
    let h = H::new();
    h.t.clock.set(now);
    h.setup();
    h.ctx().symbols.load(master());
    let (cookie, csrf) = h.session(true);
    L { h, cookie, csrf }
}

/// C-11: at 00:30 IST on 1 October 2026 the UTC date is still 30
/// September. The route counts the monthly types from the IST day:
/// current_month is October's last listed expiry and next_month has none.
/// Read from the UTC day, current_month would find nothing in September
/// and the asked type would fall back to the nearest expiry.
#[tokio::test]
async fn the_monthly_types_count_from_the_ist_day_across_midnight_utc() {
    let l = at(ist(2026, 10, 1, 0, 30));
    assert_eq!(
        l.h.ctx().now().date_naive(),
        chrono::NaiveDate::from_ymd_opt(2026, 9, 30).unwrap(),
        "the UTC day is the one before"
    );
    l.h.connect_broker();
    l.h.mock.set_quote(Quote {
        symbol: "NIFTY".into(),
        exchange: "NSE_INDEX".into(),
        ltp: 24412.0,
        ..Default::default()
    });
    let (s, v) = l
        .get("/flow/api/option-strikes?underlying=NIFTY&optionType=CE&expiryType=current_month")
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let d = &v["data"];
    assert_eq!(
        d["resolved"],
        json!({"current_week": "13OCT26", "next_week": "27OCT26",
               "current_month": "27OCT26", "next_month": null})
    );
    assert_eq!(d["expiry"], "27OCT26");
}

/// C-12: NIFTYNXT5028AUG26 never splits, its base having digits, so the
/// route asks for the expiries of the whole text and finds none, as the
/// web's route does; NIFTYNXT50's own listed contract is not used.
#[tokio::test]
async fn an_underlying_whose_name_has_digits_is_looked_up_whole() {
    let l = L::new();
    let (s, v) = l
        .get("/flow/api/option-strikes?underlying=NIFTYNXT5028AUG26&optionType=CE")
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::NOT_FOUND,
            json!({"status": "error",
                   "message": "No option expiries found for NIFTYNXT5028AUG26 on NFO"})
        )
    );
}
