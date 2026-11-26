//! 5paisa XTS: the XTS market data feed with its own configuration
//! (`brokers::fivepaisaxts::CONFIG`; the family's feed in [`super::xts`]).
//! Its cases run signed in, with the market data keys in memory, since its
//! feed connects with the stored session and never logs in while one is
//! stored (web #2484).

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::fivepaisaxts::CONFIG;

/// The market data token stored at sign-in, shaped as XTS issues it: a JWT
/// whose `userID` claim reads `<user id>_<app id>` (`{"userID":"MD1_APP"}`,
/// with a placeholder signature). The web's feed takes the socket
/// address's user id from that claim (`market_data_user_id`) and logs in
/// when a token carries none, so every case runs the web's path. The claim
/// names another user (MD1) than the one stored at sign-in (U1, `xts.rs`),
/// so the address shows it reads the claim, as the web does, and not the
/// desktop's fallback for a token without one (`Connector::known_session`).
const MARKET_TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.eyJ1c2VySUQiOiJNRDFfQVBQIn0.sig";

fn feeds(env: &Env) -> Feeds {
    super::xts::feeds_signed_in(&CONFIG, env, MARKET_TOKEN)
}

corpus_cases!(
    "fivepaisaxts",
    feeds,
    [
        depth_five_and_clear,
        depth_five_beside_twenty,
        depth_thirty_and_fifty_refused,
        depth_twenty,
        depth_zero_slots_kept,
        future_depth_oi,
        index_quote_root,
        index_quote_touchline,
        ltp_equity_and_index,
        quote_partial_keeps_the_quote,
        quote_root,
        quote_touchline,
        reconnect_keeps_already_subscribed_instruments,
        reconnect_keeps_the_market_session,
        reconnect_replays,
        server_disconnect_reconnects,
        subscribe_refused_by_the_broker,
        text_1105,
        unsubscribe_keeps_the_others,
    ]
);
