//! Kotak Neo: the SFeed market feed (`brokers::kotak::streaming::KotakFeed`)
//! and the realtime order socket (`brokers::kotak::order_feed::KotakOrderFeed`).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::kotak::order_feed::KotakOrderFeed;
use openalgo_desktop_lib::brokers::kotak::streaming::KotakFeed;
use openalgo_desktop_lib::brokers::kotak::KotakSession;

fn feeds(env: &Env) -> Feeds {
    let session = KotakSession {
        token: "t".into(),
        sid: "s".into(),
        base_url: env.url("orders"),
        access_token: String::new(),
        data_center: String::new(),
    };
    Feeds::market(KotakFeed::new(
        &env.url("main"),
        "s",
        "U".into(),
        env.symbols(),
    ))
    .with_orders(KotakOrderFeed::new(&session, env.symbols()))
}

corpus_cases!(
    "kotak",
    feeds,
    [
        quote_mode,
        ltp_mode,
        first_mini_tick,
        depth_book_replaced,
        index_two_names_one_feed,
        index_depth,
        derivative_quote_and_depth,
        reconnect_clears_the_book,
        bcd_currency_quote,
        order_lifecycle,
    ]
);
