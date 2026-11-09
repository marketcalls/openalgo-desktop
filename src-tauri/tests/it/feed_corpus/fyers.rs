//! Fyers: the HSM market feed (`HsmFeed`), the TBT 50-level depth socket
//! (`TbtFeed`) and the order socket (`OrderFeed`), on a fake session token
//! whose JWT carries the `hsm_key` HSM logs in with.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::fyers::streaming::{HsmFeed, OrderFeed, TbtFeed};
use openalgo_desktop_lib::brokers::types::AuthToken;

/// `app_id:access_token`, the access token a JWT whose payload is
/// `{"hsm_key":"hsmkey123","exp":4102444800}` (fake values).
const SESSION: &str =
    "APPID-100:eyJhbGciOiJIUzI1NiJ9.eyJoc21fa2V5IjoiaHNta2V5MTIzIiwiZXhwIjo0MTAyNDQ0ODAwfQ.c2ln";

fn feeds(env: &Env) -> Feeds {
    let auth = AuthToken::new(SESSION);
    Feeds::market(HsmFeed::new(&env.url("main"), &auth, env.symbols()).expect("the HSM feed"))
        .with_depth(
            50,
            TbtFeed::new(&env.url("depth"), &auth, env.symbols()).expect("the TBT feed"),
        )
        .with_orders(
            OrderFeed::new(&env.url("orders"), &auth, env.symbols()).expect("the order feed"),
        )
}

corpus_cases!(
    "fyers",
    feeds,
    [
        quote_ltp_snapshot_update,
        depth_before_first_quote,
        index_quote,
        order_lifecycle,
        tbt_50_depth,
    ]
);
