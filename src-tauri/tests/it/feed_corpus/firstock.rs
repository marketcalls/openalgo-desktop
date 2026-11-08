//! Firstock: one JSON feed for every mode (`brokers::firstock::streaming::
//! FirstockFeed`), on the path the live socket has (`/V2/ws`). The web
//! streams no order updates for Firstock (its handler only logs them), so
//! no order socket.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::firstock::streaming::FirstockFeed;

fn feeds(env: &Env) -> Feeds {
    let base = format!("{}/V2/ws", env.url("main"));
    Feeds::market(FirstockFeed::new(&base, "AB1234", "jk"))
}

corpus_cases!(
    "firstock",
    feeds,
    [
        quote_before_the_login_answer,
        ltp_then_quote,
        depth_future_book_clear,
        index_quote,
        reconnect_subscribes_again,
    ]
);
