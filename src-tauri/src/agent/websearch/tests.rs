//! Tests of web search: the taint boundary, the settings and keys, the
//! daily counter under concurrency (D-019), each provider against a local
//! fake (no real provider, no real key), and the two tools end to end.

mod config;
mod providers;
mod taint;
mod tool;

use super::providers::Net;
use crate::agent::outbound::fake::FakeServer;
use crate::security::Unlocked;
use crate::state::testing::TestCtx;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

pub const TAVILY_PATH: &str = "/tavily/search";
pub const PERPLEXITY_PATH: &str = "/perplexity/v1/agent";
pub const DDG_HTML: &str = "/ddg/html/";
pub const DDG_LITE: &str = "/ddg/lite/";

pub fn ctx() -> TestCtx {
    let t = crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
    t.ctx.security.install(&Unlocked::random().unwrap());
    t
}

/// Every provider on the fake, with a gate of `places`.
pub fn net_on(fake: &FakeServer, places: usize) -> Net {
    Net {
        tavily: fake.url(TAVILY_PATH),
        perplexity: fake.url(PERPLEXITY_PATH),
        duckduckgo_html: fake.url(DDG_HTML),
        duckduckgo_lite: fake.url(DDG_LITE),
        gate: Arc::new(Semaphore::new(places)),
        gate_wait: Duration::from_millis(100),
    }
}

/// Point this test thread's searches at the fake; undone on drop.
pub struct OnFake;

impl OnFake {
    pub fn new(fake: &FakeServer, places: usize) -> OnFake {
        super::providers::set_test_net(Some(net_on(fake, places)));
        OnFake
    }
}

impl Drop for OnFake {
    fn drop(&mut self) {
        super::providers::set_test_net(None);
    }
}

/// A DuckDuckGo html page with these `(href, title, snippet)` results.
pub fn ddg_html(rows: &[(&str, &str, &str)]) -> String {
    let mut out = String::from("<html><body><div id=\"links\">");
    for (href, title, snippet) in rows {
        out.push_str(&format!(
            "<div class=\"result results_links web-result\"><div class=\"links_main result__body\">\
             <h2 class=\"result__title\"><a rel=\"nofollow\" class=\"result__a\" href=\"{}\">{}</a></h2>\
             <a class=\"result__snippet\" href=\"{}\">{}</a></div></div>",
            href, title, href, snippet
        ));
    }
    out.push_str("</div></body></html>");
    out
}
