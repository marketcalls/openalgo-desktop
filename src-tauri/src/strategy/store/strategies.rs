//! Strategy rows: create, read and list, the webhook token lookups and the
//! status reads (web `strategy_module_db.py` strategy functions).

use super::{
    fnum, generate_webhook_token, hash_webhook_token, iso, json_col, num_param, strategy_from,
    time_col, Store, StrategyRow, STRATEGY_COLS,
};
use crate::error::{AppError, Result};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

impl Store {
    /// Create a strategy and issue its webhook token, returned once.
    pub fn create_strategy(&self, user_id: &str, config: &Value) -> Result<(StrategyRow, String)> {
        let name = config["name"].as_str().unwrap_or_default().to_string();
        let conn = self.conn()?;
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sm_strategy WHERE user_id = ?1 AND name = ?2)",
            params![user_id, name],
            |r| r.get(0),
        )?;
        if exists {
            return Err(AppError::Validation(format!(
                "A strategy named '{}' already exists",
                name
            )));
        }
        let token = generate_webhook_token();
        let now = self.utcnow();
        let s = |k: &str, d: &str| {
            config
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or(d)
                .to_string()
        };
        conn.execute(
            "INSERT INTO sm_strategy (user_id, name, strategy_kind, direction, universe_tab, \
             underlying, underlying_exchange, strategy_type, entry_time, exit_time, product, \
             pricetype, legs, overall_sl_mtm, overall_target_mtm, lock_profit, trail_sl_to_entry, \
             scheduler, live_enabled, webhook_token_hash, webhook_ip_allowlist, webhook_locked, \
             daily_loss_limit_inr, status, current_run_id, created_at, updated_at) VALUES \
             (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, 0, \
              ?19, ?20, 0, ?21, 'stopped', NULL, ?22, ?22)",
            params![
                user_id,
                name,
                s("strategy_kind", "batch"),
                s("direction", "both"),
                s("universe_tab", "weekly_monthly"),
                s("underlying", ""),
                s("underlying_exchange", ""),
                s("strategy_type", "intraday"),
                time_col(&config["entry_time"]),
                time_col(&config["exit_time"]),
                s("product", "NRML"),
                s("pricetype", "MARKET"),
                config.get("legs").cloned().unwrap_or(json!([])).to_string(),
                num_param(&config["overall_sl_mtm"]),
                num_param(&config["overall_target_mtm"]),
                json_col(&config["lock_profit"]),
                config["trail_sl_to_entry"].as_bool().unwrap_or(false),
                json_col(&config["scheduler"]),
                hash_webhook_token(&token),
                json_col(&config["webhook_ip_allowlist"]),
                num_param(&config["daily_loss_limit_inr"]),
                now,
            ],
        )?;
        let id = conn.last_insert_rowid();
        drop(conn);
        let row = self
            .get_strategy_unscoped(id)?
            .ok_or_else(|| AppError::Internal("created strategy vanished".into()))?;
        Ok((row, token))
    }

    /// Every strategy for a user, newest first, with its last finalised run.
    pub fn list_strategies(
        &self,
        user_id: &str,
        status: Option<&str>,
        q: Option<&str>,
    ) -> Result<Vec<Value>> {
        let conn = self.conn()?;
        let mut sql = format!(
            "SELECT {} FROM sm_strategy WHERE user_id = ?1",
            STRATEGY_COLS
        );
        let mut args: Vec<String> = vec![user_id.to_string()];
        if let Some(s) = status {
            args.push(s.to_string());
            sql.push_str(&format!(" AND status = ?{}", args.len()));
        }
        if let Some(q) = q {
            args.push(format!(
                "%{}%",
                q.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            ));
            sql.push_str(&format!(
                " AND name LIKE ?{} ESCAPE '\\' COLLATE NOCASE",
                args.len()
            ));
        }
        sql.push_str(" ORDER BY created_at DESC, id DESC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args.iter()), strategy_from)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let mut data = row.to_dict(false);
            let last: Option<(i64, Option<f64>, Option<String>)> = conn
                .query_row(
                    "SELECT id, pnl_realized, stopped_at FROM sm_strategy_run \
                     WHERE strategy_id = ?1 AND stopped_at IS NOT NULL \
                     ORDER BY stopped_at DESC, id DESC LIMIT 1",
                    [row.id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            data["last_finalized_run"] = match last {
                None => Value::Null,
                Some((id, pnl, stopped)) => json!({
                    "id": id,
                    "pnl_realized": fnum(pnl.unwrap_or(0.0)),
                    "stopped_at": iso(stopped.as_deref()),
                }),
            };
            out.push(data);
        }
        Ok(out)
    }

    /// One strategy, scoped to its owner.
    pub fn get_strategy(&self, strategy_id: i64, user_id: &str) -> Result<Option<StrategyRow>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {} FROM sm_strategy WHERE id = ?1 AND user_id = ?2",
                    STRATEGY_COLS
                ),
                params![strategy_id, user_id],
                strategy_from,
            )
            .optional()?)
    }

    /// One strategy without an owner filter. Engine only.
    pub fn get_strategy_unscoped(&self, strategy_id: i64) -> Result<Option<StrategyRow>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                &format!("SELECT {} FROM sm_strategy WHERE id = ?1", STRATEGY_COLS),
                [strategy_id],
                strategy_from,
            )
            .optional()?)
    }

    /// The names of the strategies whose webhook takes alerts (not
    /// locked), by name: what quitting leaves refusing them (ARCH-03).
    pub fn alert_strategy_names(&self) -> Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT name FROM sm_strategy WHERE webhook_locked = 0 ORDER BY name")?;
        let names = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(names)
    }

    /// Every strategy id (scheduler sync).
    pub fn all_strategy_ids(&self) -> Result<Vec<i64>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT id FROM sm_strategy ORDER BY id")?;
        let ids = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        Ok(ids)
    }

    pub fn set_strategy_status(
        &self,
        strategy_id: i64,
        status: &str,
        run_id: Option<i64>,
    ) -> Result<bool> {
        let conn = self.conn()?;
        Ok(conn.execute(
            "UPDATE sm_strategy SET status = ?1, current_run_id = ?2 WHERE id = ?3",
            params![status, run_id, strategy_id],
        )? == 1)
    }

    /// Resolve an inbound webhook token through its digest (unique index).
    pub fn get_strategy_by_webhook_token(&self, token: &str) -> Result<Option<StrategyRow>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {} FROM sm_strategy WHERE webhook_token_hash = ?1",
                    STRATEGY_COLS
                ),
                [hash_webhook_token(token)],
                strategy_from,
            )
            .optional()?)
    }
}
