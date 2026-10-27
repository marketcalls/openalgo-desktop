//! Tests of the ChatGPT plan, against a local fake of OpenAI's sign-in
//! service and of the Codex backend (no real OpenAI, no real key). Ported
//! from the web's `test_agent_chatgpt_oauth.py`, `..._wiring.py` and
//! `..._models.py`, plus the desktop's fixes (D-023, D-025, D-026).

mod codex;
mod credential;
mod flow;
mod models;

use super::flow::{Endpoints, Flow};
use super::record::{self, AuthRecord};
use super::signin::SignIn;
use super::ChatGptPlan;
use crate::agent::outbound::fake::{Answer, FakeServer};
use crate::security::{Secret, Unlocked};
use crate::state::testing::TestCtx;
use base64::Engine;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

pub const ACCOUNT: &str = "acct-test-0001";

pub fn ctx() -> TestCtx {
    let t = crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
    t.ctx.security.install(&Unlocked::random().unwrap());
    t
}

/// An unsigned token carrying `exp` and, optionally, the account claim.
pub fn jwt(exp: i64, account: Option<&str>, salt: &str) -> String {
    let b64 = |v: Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string());
    let mut claims = json!({"exp": exp, "salt": salt});
    if let Some(a) = account {
        claims["https://api.openai.com/auth"] = json!({"chatgpt_account_id": a});
    }
    format!("{}.{}.sig", b64(json!({"alg": "none"})), b64(claims))
}

pub fn now_s(t: &TestCtx) -> i64 {
    t.ctx.now().timestamp()
}

/// A quick flow against the fake.
pub fn flow_at(fake: &FakeServer, lifetime: Duration) -> Flow {
    Flow {
        endpoints: Endpoints::at(&fake.url("")),
        lifetime,
        interval: Duration::from_millis(10),
    }
}

/// Install a plan on the fake and hand back its sign-in.
pub fn install(t: &TestCtx, flow: Flow) -> Arc<SignIn> {
    t.ctx.agent.set_plan(Arc::new(ChatGptPlan::new(flow)));
    super::sign_in_of(&t.ctx).unwrap()
}

pub const CODE: &str = "/api/accounts/deviceauth/usercode";
pub const POLL: &str = "/api/accounts/deviceauth/token";
pub const TOKEN: &str = "/oauth/token";

/// The fake's tokens for an approved sign-in.
pub fn tokens(t: &TestCtx, salt: &str) -> Value {
    let exp = now_s(t) + 3600;
    json!({
        "access_token": jwt(exp, Some(ACCOUNT), &format!("access-{}", salt)),
        "refresh_token": format!("rt-{}-0123456789", salt),
        "id_token": jwt(exp, Some(ACCOUNT), &format!("id-{}", salt)),
    })
}

/// A sign-in service that issues `user_code`, says "not yet" `waits`
/// times, then approves.
pub async fn auth_server(t: &TestCtx, user_code: &str, waits: usize) -> FakeServer {
    let fake = FakeServer::start().await;
    fake.script(
        CODE,
        vec![Answer::json(
            200,
            json!({"device_auth_id": "dev-1", "user_code": user_code, "interval": "0"}),
        )],
    );
    let mut polls: Vec<Answer> = (0..waits).map(|_| Answer::json(403, json!({}))).collect();
    polls.push(Answer::json(
        200,
        json!({"authorization_code": "auth-code-1", "code_challenge": "ch", "code_verifier": "ver-1"}),
    ));
    fake.script(POLL, polls);
    fake.script(TOKEN, vec![Answer::json(200, tokens(t, "one"))]);
    fake
}

/// Wait (bounded) for a condition the code under test makes true.
pub async fn until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {}",
            what
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The stored record, if any.
pub fn stored(t: &TestCtx) -> Option<AuthRecord> {
    record::load(&t.ctx.sqlite.conn().unwrap(), &t.ctx.security)
}

/// Store a record directly.
pub fn put(t: &TestCtx, r: &AuthRecord) {
    record::store(&t.ctx.sqlite.conn().unwrap(), &t.ctx.security, r).unwrap();
}

/// A record whose access token expires `in_s` seconds from the clock.
pub fn record_expiring(t: &TestCtx, in_s: i64, refresh: Option<&str>) -> AuthRecord {
    let exp = now_s(t) + in_s;
    AuthRecord::from_tokens(
        Secret::new(jwt(exp, Some(ACCOUNT), "stored")),
        refresh.map(Secret::new),
        Secret::new(jwt(exp, Some(ACCOUNT), "stored-id")),
    )
}
