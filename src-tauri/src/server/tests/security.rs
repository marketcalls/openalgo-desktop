//! Regression tests for the 2026-10-09 security review
//! (`docs/audit/2026-10-09/01-security.md`). Each test names its finding.

use super::*;

fn count(h: &H, sql: &str) -> i64 {
    h.ctx()
        .sqlite
        .conn()
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

// ------------------------------------------------- S-01 account reset

/// S-01: the account cannot be reset over HTTP any more, whatever the
/// caller sends (a session, its CSRF token and the confirmation).
#[tokio::test]
async fn s01_account_reset_is_not_reachable_over_http() {
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    let (cookie, csrf) = h.session(false);
    let (s, _) = h
        .json(with_session(
            post_json("/auth/reset-account", json!({"confirm": "RESET"})),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(!AuthService::needs_setup(h.ctx()).unwrap(), "account kept");
    assert!(h.ctx().is_broker_connected(), "broker session kept");
    let (s, _) = h
        .json(post_json("/api/v1/ping", json!({"apikey": key})))
        .await;
    assert_eq!(s, StatusCode::OK, "API key kept");
}

/// S-07: a reset leaves nothing usable behind: MCP tokens, strategy and
/// Chartink webhook secrets, Telegram and WhatsApp links.
#[tokio::test]
async fn s07_account_reset_revokes_every_outside_credential() {
    let h = H::new();
    h.setup();
    h.connect_broker();
    let now = h.ctx().now();
    let (mcp_token, strategy_hash) = {
        let c = h.ctx().sqlite.conn().unwrap();
        let (_, mcp_token) = crate::mcp::store::create_token(
            &c,
            "claude",
            crate::mcp::store::TokenScope::ReadWrite,
            now,
        )
        .unwrap();
        let token = crate::strategy::store::generate_webhook_token();
        let strategy_hash = crate::strategy::store::hash_webhook_token(&token);
        c.execute(
            "INSERT INTO sm_strategy (user_id, name, universe_tab, underlying,
                 underlying_exchange, webhook_token_hash, created_at, updated_at)
             VALUES ('trader', 's1', 'index', 'NIFTY', 'NSE_INDEX', ?1, 'x', 'x')",
            [&strategy_hash],
        )
        .unwrap();
        c.execute(
            "INSERT INTO chartink_strategies (name, webhook_id)
             VALUES ('scan', '11111111-1111-4111-8111-111111111111')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO telegram_users (telegram_id, openalgo_username) VALUES (42, 'trader')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO whatsapp_users (whatsapp_jid, phone_number, openalgo_username)
             VALUES ('919@s.whatsapp.net', '919', 'trader')",
            [],
        )
        .unwrap();
        (mcp_token, strategy_hash)
    };

    AuthService::reset_account_everywhere(h.ctx())
        .await
        .unwrap();

    let c = h.ctx().sqlite.conn().unwrap();
    assert!(crate::mcp::store::find_token(&c, &mcp_token)
        .unwrap()
        .is_none());
    let hash: String = c
        .query_row("SELECT webhook_token_hash FROM sm_strategy", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_ne!(hash, strategy_hash, "strategy webhook rotated");
    let id: String = c
        .query_row("SELECT webhook_id FROM chartink_strategies", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_ne!(id, "11111111-1111-4111-8111-111111111111");
    assert!(uuid::Uuid::parse_str(&id).is_ok());
    drop(c);
    assert_eq!(count(&h, "SELECT COUNT(*) FROM telegram_users"), 0);
    assert_eq!(count(&h, "SELECT COUNT(*) FROM whatsapp_users"), 0);
    assert_eq!(count(&h, "SELECT COUNT(*) FROM users"), 0);
    assert!(!h.ctx().is_broker_connected());
    assert!(AuthService::needs_setup(h.ctx()).unwrap());
    // Strategy and Chartink definitions are kept.
    assert_eq!(count(&h, "SELECT COUNT(*) FROM sm_strategy"), 1);
    assert_eq!(count(&h, "SELECT COUNT(*) FROM chartink_strategies"), 1);
}
