//! The live feed's step before a new instrument (`Broker::feed_presubscribe`,
//! audit RB-05). Samco streams an index under its `listingId`, which the
//! master does not carry; the web looks it up over REST as it subscribes
//! (`samco_adapter.py` `subscribe`, `_get_index_listing_id`), so a cold
//! index streams without a REST quote first. Ids are cached in the
//! broker's bounded `ListingIds`, which the REST quote shares.

use super::{data, SamcoBroker};
use crate::brokers::common::streaming::{FeedSubscription, Presubscribe};
use crate::brokers::types::{AuthToken, QuoteKey};
use futures_util::FutureExt;
use std::sync::Arc;

/// The step for the session signed in with `auth`.
pub(super) fn step(broker: &SamcoBroker, auth: &AuthToken) -> Presubscribe {
    // The step's copy gets its own empty poller slot so it never keeps the
    // order poller (and itself) alive.
    let mut core = broker.clone();
    core.poller = Arc::default();
    let auth = auth.clone();
    Arc::new(move |subs: Vec<FeedSubscription>| {
        let (core, auth) = (core.clone(), auth.clone());
        async move {
            let mut out = Vec::with_capacity(subs.len());
            for s in subs {
                out.push(listed(&core, &auth, s).await);
            }
            out
        }
        .boxed()
    })
}

/// `s` as the feed streams it: an index under its listing id, anything
/// else as the master describes it; or why not, in the web's words
/// (`samco_adapter.py:255`).
async fn listed(
    core: &SamcoBroker,
    auth: &AuthToken,
    s: FeedSubscription,
) -> Result<FeedSubscription, String> {
    if !matches!(s.exchange.as_str(), "NSE_INDEX" | "BSE_INDEX") {
        return Ok(s);
    }
    let key = QuoteKey::new(&s.exchange, &s.symbol);
    match data::index_listing_id(core, auth, &key).await {
        Ok(id) => Ok(FeedSubscription { token: id, ..s }),
        Err(e) => Err(format!(
            "Error getting index listingId: {}",
            e.client_message()
        )),
    }
}
