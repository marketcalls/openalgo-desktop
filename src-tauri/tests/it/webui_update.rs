//! In-app updates over HTTP (`/api/update/*`): signed-in only, CSRF on the
//! POSTs, notify only (nothing downloads before Update), the trader asked
//! before a restart interrupts Live or sandbox activity, and a copy that
//! cannot replace itself (a `.deb`) sent to the release page. The update
//! plugin is replaced by a scripted backend; `crate::updater`'s own tests
//! drive the real plugin against a local update address.

use crate::webui_support;

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use openalgo_desktop_lib::services::update::{
    Activity, Decision, Offer, Package, ProgressFn, UpdateBackend, UpdateFailure,
};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use webui_support::*;

/// A scripted update plugin: answers the check with `answer`, and counts
/// downloads (each one "verifies" at once).
struct FakeBackend {
    package: Package,
    answer: Mutex<Result<Option<Offer>, UpdateFailure>>,
    downloads: AtomicUsize,
    discards: AtomicUsize,
}

impl FakeBackend {
    fn offering(package: Package, version: &str) -> Arc<Self> {
        Arc::new(Self {
            package,
            answer: Mutex::new(Ok(Some(Offer::new(
                version,
                Some("Faster option chain.".into()),
                Some("2026-10-15T04:30:00Z".into()),
            )))),
            downloads: AtomicUsize::new(0),
            discards: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl UpdateBackend for FakeBackend {
    fn package(&self) -> Package {
        self.package
    }
    async fn check(&self) -> Result<Option<Offer>, UpdateFailure> {
        self.answer.lock().clone()
    }
    async fn download(
        &self,
        _version: &str,
        mut progress: ProgressFn,
    ) -> Result<(), UpdateFailure> {
        self.downloads.fetch_add(1, Ordering::SeqCst);
        progress(100, Some(100));
        Ok(())
    }
    fn discard(&self) {
        self.discards.fetch_add(1, Ordering::SeqCst);
    }
}

fn post(path: &str, body: Value) -> axum::http::Request<axum::body::Body> {
    req(Method::POST, path, Some(body))
}

async fn wait_for_state(h: &H, c: &str, want: &str) -> Value {
    for _ in 0..200 {
        let (_, v) = h.json(with(get("/api/update/status"), c, None)).await;
        if v["data"]["state"] == want {
            return v["data"].clone();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the update never reached {}", want);
}

fn add_stop(h: &H, symbol: &str, mode: &str) {
    let conn = h.ctx.sqlite.conn().unwrap();
    conn.execute(
        "INSERT INTO scalping_sl_state (symbol, exchange, product, side, mode, entry_price, quantity, is_active) \
         VALUES (?1, 'NSE', 'MIS', 'BUY', ?2, 800.0, 1, 1)",
        rusqlite::params![symbol, mode],
    )
    .unwrap();
}

#[tokio::test]
async fn update_routes_need_the_signed_in_user_and_the_csrf_token() {
    let h = H::new();
    h.setup();
    let (anon, anon_csrf) = h.session(false);
    let (s, v) = h.json(with(get("/api/update/status"), &anon, None)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(
        v,
        json!({"status": "error", "message": "Not authenticated"})
    );
    for p in [
        "/api/update/check",
        "/api/update/install",
        "/api/update/cancel",
    ] {
        let (s, _) = h
            .json(with(post(p, json!({})), &anon, Some(&anon_csrf)))
            .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{}", p);
    }
    // Signed in, without the CSRF token: refused before anything runs.
    let backend = FakeBackend::offering(Package::MacApp, "9.0.0");
    h.ctx.updates.set_backend(backend.clone());
    let (user, _) = h.session(true);
    for p in [
        "/api/update/check",
        "/api/update/install",
        "/api/update/cancel",
    ] {
        let (s, v) = h.json(with(post(p, json!({})), &user, None)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}", p);
        assert_eq!(v["status"], "error", "{}", p);
    }
    assert_eq!(h.ctx.updates.status().state, "not_checked");
    assert_eq!(backend.downloads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_check_offers_the_update_and_nothing_downloads_before_update() {
    let (h, c, t) = signed_in();
    let backend = FakeBackend::offering(Package::MacApp, "9.0.0");
    h.ctx.updates.set_backend(backend.clone());

    let (s, v) = h.json(with(get("/api/update/status"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["data"]["state"], "not_checked");
    assert_eq!(v["data"]["current_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["data"]["can_install"], true);
    assert_eq!(v["data"]["package"], "mac_app");

    let (s, v) = h
        .json(with(post("/api/update/check", json!({})), &c, Some(&t)))
        .await;
    assert_eq!(s, StatusCode::OK);
    let d = &v["data"];
    assert_eq!(d["state"], "available");
    assert_eq!(d["offer"]["version"], "9.0.0");
    assert_eq!(d["offer"]["notes"], "Faster option chain.");
    assert_eq!(
        d["offer"]["release_url"],
        "https://github.com/marketcalls/openalgo-desktop/releases/tag/v9.0.0"
    );
    assert!(d["checked_at"].is_string());
    // Notify only: a check downloads nothing.
    assert_eq!(backend.downloads.load(Ordering::SeqCst), 0);

    // Nothing runs: Update downloads and verifies straight away.
    let (s, v) = h
        .json(with(post("/api/update/install", json!({})), &c, Some(&t)))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["status"], "success");
    let ready = wait_for_state(&h, &c, "ready").await;
    assert_eq!(ready["offer"]["version"], "9.0.0");
    assert_eq!(backend.downloads.load(Ordering::SeqCst), 1);

    // A second Update while one is downloaded changes nothing.
    let (s, _) = h
        .json(with(post("/api/update/install", json!({})), &c, Some(&t)))
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(backend.downloads.load(Ordering::SeqCst), 1);

    // Later forgets the download and offers the update again.
    let (_, v) = h
        .json(with(post("/api/update/cancel", json!({})), &c, Some(&t)))
        .await;
    assert_eq!(v["data"]["state"], "available");
    assert_eq!(backend.discards.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn update_asks_first_while_live_trading_runs() {
    let (h, c, t) = signed_in();
    let backend = FakeBackend::offering(Package::WindowsNsis, "9.0.0");
    h.ctx.updates.set_backend(backend.clone());
    h.json(with(post("/api/update/check", json!({})), &c, Some(&t)))
        .await;
    add_stop(&h, "SBIN", "live");
    add_stop(&h, "INFY", "analyze");

    let (s, v) = h
        .json(with(post("/api/update/install", json!({})), &c, Some(&t)))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "confirm");
    assert_eq!(v["warning"]["level"], "live");
    assert_eq!(v["warning"]["title"], "Live trading is running");
    let items: Vec<String> = serde_json::from_value(v["warning"]["items"].clone()).unwrap();
    assert!(
        items[0].starts_with("1 Live position under scalping protection."),
        "{:?}",
        items
    );
    assert!(items
        .iter()
        .any(|i| i.contains("Sandbox: 1 sandbox position under scalping protection")));
    assert_eq!(v["activity"]["live_scalping_stops"], 1);
    assert_eq!(
        backend.downloads.load(Ordering::SeqCst),
        0,
        "nothing before the confirm"
    );
    assert_eq!(h.ctx.updates.status().state, "available");

    // A confirmation of less than what runs now is asked again.
    let stale = json!({"confirmed": {"live_scalping_stops": 0, "sandbox_scalping_stops": 1}});
    let (_, again) = h
        .json(with(post("/api/update/install", stale), &c, Some(&t)))
        .await;
    assert_eq!(again["status"], "confirm");
    assert_eq!(backend.downloads.load(Ordering::SeqCst), 0);

    // The activity the trader saw, sent back, goes ahead.
    let confirmed = json!({"confirmed": v["activity"].clone()});
    let (s, ok) = h
        .json(with(post("/api/update/install", confirmed), &c, Some(&t)))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(ok["status"], "success", "{}", ok);
    wait_for_state(&h, &c, "ready").await;

    // The restart asks again when something new started since: a second
    // Live stop is not covered by what was confirmed.
    let seen: Activity = serde_json::from_value(v["activity"].clone()).unwrap();
    add_stop(&h, "TCS", "live");
    match h.ctx.updates.begin_restart(&h.ctx, Some(seen)).await {
        Decision::Confirm { activity, warning } => {
            assert_eq!(activity.live_scalping_stops, 2);
            assert_eq!(warning.level, "live");
        }
        other => panic!("expected a new confirmation, got {:?}", other),
    }
    assert_eq!(h.ctx.updates.status().state, "ready");
    // Confirmed again with the new activity: the restart goes ahead.
    let now = Activity {
        live_scalping_stops: 2,
        ..seen
    };
    match h.ctx.updates.begin_restart(&h.ctx, Some(now)).await {
        Decision::Proceed(st) => assert_eq!(st.state, "restarting"),
        other => panic!("expected the restart, got {:?}", other),
    }
}

/// ARCH-03: quitting reads the same inventory as an update's restart and
/// names it in quitting's words, with the key lock last (KEY-01).
#[tokio::test]
async fn quitting_names_the_protection_that_stops() {
    use openalgo_desktop_lib::services::lifecycle::quit::quit_warning;
    let (h, _, _) = signed_in();
    assert_eq!(quit_warning(&h.ctx).await, None, "nothing runs");
    add_stop(&h, "SBIN", "live");
    add_stop(&h, "INFY", "analyze");
    let w = quit_warning(&h.ctx).await.unwrap();
    assert_eq!(w.level, "live");
    assert!(
        w.intro.starts_with("Quitting closes OpenAlgo."),
        "{}",
        w.intro
    );
    assert!(
        w.items[0].starts_with("1 Live position under scalping protection."),
        "{:?}",
        w.items
    );
    assert!(w.items[1].starts_with("Sandbox: 1 sandbox position under scalping protection"));
    assert!(w.items[2].starts_with("OpenAlgo locks its keys when it closes"));
}

#[tokio::test]
async fn sandbox_only_activity_gets_the_lighter_warning() {
    let (h, c, t) = signed_in();
    h.ctx
        .updates
        .set_backend(FakeBackend::offering(Package::LinuxAppImage, "9.0.0"));
    h.json(with(post("/api/update/check", json!({})), &c, Some(&t)))
        .await;
    add_stop(&h, "SBIN", "analyze");
    let (_, v) = h
        .json(with(post("/api/update/install", json!({})), &c, Some(&t)))
        .await;
    assert_eq!(v["status"], "confirm");
    assert_eq!(v["warning"]["level"], "sandbox");
    assert_eq!(v["warning"]["title"], "Sandbox activity is running");
    assert_eq!(
        v["warning"]["items"],
        json!(["1 sandbox position under scalping protection."])
    );
}

#[tokio::test]
async fn a_deb_copy_is_told_about_the_update_and_never_installs_it() {
    let (h, c, t) = signed_in();
    let backend = FakeBackend::offering(Package::LinuxDeb, "9.0.0");
    h.ctx.updates.set_backend(backend.clone());
    let (_, v) = h
        .json(with(post("/api/update/check", json!({})), &c, Some(&t)))
        .await;
    let d = &v["data"];
    assert_eq!(d["state"], "available");
    assert_eq!(d["can_install"], false);
    assert_eq!(d["package"], "linux_deb");
    assert!(d["manual_reason"]
        .as_str()
        .unwrap()
        .contains("installed from a .deb package, which OpenAlgo cannot update by itself"));
    let (s, v) = h
        .json(with(post("/api/update/install", json!({})), &c, Some(&t)))
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(v["message"].as_str().unwrap().contains(".deb package"));
    assert_eq!(backend.downloads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_failed_check_is_reported_in_a_traders_words() {
    let (h, c, t) = signed_in();
    let backend = FakeBackend::offering(Package::MacApp, "9.0.0");
    *backend.answer.lock() = Err(UpdateFailure::Network("dns error: no such host".into()));
    h.ctx.updates.set_backend(backend);
    let (_, v) = h
        .json(with(post("/api/update/check", json!({})), &c, Some(&t)))
        .await;
    assert_eq!(v["data"]["state"], "failed");
    assert_eq!(
        v["data"]["message"],
        "OpenAlgo could not reach the update server. Check your internet connection and try again."
    );
    assert!(v["data"]["offer"].is_null());
    // Nothing to install.
    let (s, _) = h
        .json(with(post("/api/update/install", json!({})), &c, Some(&t)))
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[tokio::test]
async fn the_newest_version_is_reported_as_up_to_date() {
    let (h, c, t) = signed_in();
    let backend = FakeBackend::offering(Package::MacApp, "9.0.0");
    *backend.answer.lock() = Ok(None);
    h.ctx.updates.set_backend(backend);
    let (_, v) = h
        .json(with(post("/api/update/check", json!({})), &c, Some(&t)))
        .await;
    assert_eq!(v["data"]["state"], "up_to_date");
    assert!(v["data"]["offer"].is_null());
}
