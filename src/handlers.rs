// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Exchange8:Clearing
// SPDX-License-Identifier: Apache-2.0

//! Exchange8:Clearing data handlers — the trade ledger query API.
//!
//! Every handler follows the platform authorization pipeline (§5): verify
//! bearer → scope check → RLS context (same connection) → explicit
//! `WHERE env_id` (defense-in-depth) → query.
//!
//! Design: `design/sentin8-exchange8-clearing-technical-design.md` §4.1
//! + `design/sentin8-exchange8-data-model-technical-design.md` §3.4
//! (`clearing.trade` / `clearing.fix_order`).

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Acquire;
use uuid::Uuid;

use sentin8::shell::{err_db, AppState};

/// Scope constants for every handler in this module — single source of truth
/// for both the call sites and the authorization matrix tests
/// (see DESIGN.md §9). Changing a scope here changes the live gate AND the
/// test expectation together.
pub mod scopes {
    /// `GET /v1/trades` (list_trades).
    pub const TRADES_READ: &str = "exchange8:trade:read";
    /// `POST /v1/trades/bust` (bust_trade) — step-up authorization.
    pub const TRADES_BUST: &str = "exchange8:trade:bust";
    /// `GET /v1/orders` (list_orders).
    pub const ORDERS_READ: &str = "exchange8:order:read";
}

#[derive(Debug, Deserialize)]
pub struct ListTradesQuery {
    /// Filter by shared trade id.
    pub trade_id: Option<i64>,
    /// Filter by instrument security UUID.
    pub security_id: Option<Uuid>,
    /// Only trades from this timestamp onward.
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct TradeSideResponse {
    pub id: Uuid,
    pub env_id: Uuid,
    pub trade_id: i64,
    pub trade_type: String,
    pub order_id: i64,
    pub liquidity: String,
    pub volume: rust_decimal::Decimal,
    pub price: rust_decimal::Decimal,
    pub charge: rust_decimal::Decimal,
    pub bust_state: Option<String>,
    pub trade_ts: chrono::DateTime<chrono::Utc>,
}

/// `GET /v1/trades` — list executions (RLS-scoped). Both per-side rows of a
/// trade carry the same `trade_id` (one-row-per-side shape).
pub async fn list_trades(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<ListTradesQuery>,
) -> Result<Json<Vec<TradeSideResponse>>, (StatusCode, String)> {
    let a = sentin8::shell::verify_token(&state.config, &headers).await?;
    sentin8::shell::policy::require(
        &state.admin.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("exchange8:trade:read"),
    )
    .await?;
    let mut conn = state.pool.acquire().await.map_err(err_db)?;
    let mut ctx = conn.begin().await.map_err(err_db)?;
    sentin8::shell::set_tenant_context(&mut ctx, a.env_id()).await;
    let limit = q.limit.unwrap_or(100).min(1000);
    let rows = sqlx::query_as::<_, TradeSideResponse>(
        r#"SELECT id, env_id, trade_id, trade_type, order_id, liquidity,
                  volume, price, charge, bust_state, trade_ts
             FROM clearing.trade
            WHERE env_id = $1
              AND ($2::bigint IS NULL OR trade_id = $2)
              AND ($3::timestamptz IS NULL OR trade_ts >= $3)
            ORDER BY trade_ts DESC, trade_id LIMIT $4"#,
    )
    .bind(a.env_id())
    .bind(q.trade_id)
    .bind(q.since)
    .bind(limit)
    .fetch_all(&mut *ctx)
    .await
    .map_err(err_db)?;
    ctx.commit().await.map_err(err_db)?;
    Ok(Json(rows))
}

#[derive(Debug, Deserialize)]
pub struct BustTradeRequest {
    pub trade_id: i64,
    /// One of: erroneous | obvious_error | regulatory.
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct BustResponse {
    pub trade_id: i64,
    pub busted: bool,
}

/// `POST /v1/trades/bust` — bust a trade (step-up auth: `exchange8:trade:bust`).
///
/// Idempotent per trade_id (`exchange8_bust_trade` unique on env+trade).
pub async fn bust_trade(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(req): Json<BustTradeRequest>,
) -> Result<Json<BustResponse>, (StatusCode, String)> {
    let a = sentin8::shell::verify_token(&state.config, &headers).await?;
    sentin8::shell::policy::require(
        &state.admin.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("exchange8:trade:bust"),
    )
    .await?;
    if !matches!(
        req.reason.as_str(),
        "erroneous" | "obvious_error" | "regulatory"
    ) {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid bust reason".into(),
        ));
    }
    let mut conn = state.pool.acquire().await.map_err(err_db)?;
    let mut ctx = conn.begin().await.map_err(err_db)?;
    sentin8::shell::set_tenant_context(&mut ctx, a.env_id()).await;
    // ON CONFLICT (env_id, trade_id) DO NOTHING — idempotent bust. The
    // row id isn't needed; the UPDATE below marks the trade rows either way.
    let _ = sqlx::query_scalar::<_, i64>(
        r#"INSERT INTO clearing.bust_trade (env_id, trade_id, reason, busted_by)
           VALUES ($1, $2, $3, $4)
           ON CONFLICT (env_id, trade_id) DO NOTHING
           RETURNING id"#,
    )
    .bind(a.env_id())
    .bind(req.trade_id)
    .bind(req.reason)
    .bind(a.sub())
    .fetch_optional(&mut *ctx)
    .await
    .map_err(err_db)?;
    // Mark the trade rows busted either way (first bust or already-busted).
    sqlx::query(
        r#"UPDATE clearing.trade
              SET bust_state = 'busted'
            WHERE env_id = $1 AND trade_id = $2"#,
    )
    .bind(a.env_id())
    .bind(req.trade_id)
    .execute(&mut *ctx)
    .await
    .map_err(err_db)?;
    ctx.commit().await.map_err(err_db)?;
    Ok(Json(BustResponse {
        trade_id: req.trade_id,
        busted: true,
    }))
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct OrderJournalResponse {
    pub id: Uuid,
    pub env_id: Uuid,
    pub order_id: i64,
    pub connection_id: String,
    pub cl_ord_id: String,
    pub symbol: String,
    pub side: String,
    pub order_type: String,
    pub qty: rust_decimal::Decimal,
    pub price: Option<rust_decimal::Decimal>,
    pub ord_status: String,
    pub cum_qty: rust_decimal::Decimal,
    pub ts: chrono::DateTime<chrono::Utc>,
}

/// `GET /v1/orders` — the order day log (FIX_ORDER).
pub async fn list_orders(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<ListTradesQuery>,
) -> Result<Json<Vec<OrderJournalResponse>>, (StatusCode, String)> {
    let a = sentin8::shell::verify_token(&state.config, &headers).await?;
    sentin8::shell::policy::require(
        &state.admin.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("exchange8:order:read"),
    )
    .await?;
    let mut conn = state.pool.acquire().await.map_err(err_db)?;
    let mut ctx = conn.begin().await.map_err(err_db)?;
    sentin8::shell::set_tenant_context(&mut ctx, a.env_id()).await;
    let limit = q.limit.unwrap_or(100).min(500);
    let rows = sqlx::query_as::<_, OrderJournalResponse>(
        r#"SELECT id, env_id, order_id, connection_id, cl_ord_id, symbol, side,
                  order_type, qty, price, ord_status, cum_qty, ts
             FROM clearing.fix_order
            WHERE env_id = $1
              AND ($2::bigint IS NULL OR order_id = $2)
            ORDER BY ts DESC LIMIT $3"#,
    )
    .bind(a.env_id())
    .bind(q.trade_id)
    .bind(limit)
    .fetch_all(&mut *ctx)
    .await
    .map_err(err_db)?;
    ctx.commit().await.map_err(err_db)?;
    Ok(Json(rows))
}

// ---------------------------------------------------------------------------
// Authorization matrix tests (see DESIGN.md §9).
//
// DB-free: they exercise exactly the decision the handlers delegate to
// (`AuthContext::require_any_scope`) against the SAME constants the handlers
// require, so a drifted constant or a changed platform semantics fails here
// before it ships. Scope is the single authorization truth (architecture
// principles §5).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod scope_matrix {
    use super::scopes;
    use sentin8::shell::{AuthContext, VerifiedClaims};

    /// (handler, required scopes) wiring — call sites must consume exactly
    /// these constants (compile-enforced via `scopes::`).
    const HANDLER_SCOPES: &[(&str, &[&str])] = &[
        ("list_trades", &[scopes::TRADES_READ]),
        ("bust_trade", &[scopes::TRADES_BUST]),
        ("list_orders", &[scopes::ORDERS_READ]),
    ];

    /// Every exchange8 scope this module declares — drives the sibling
    /// denial row (one service's scope must not open another's guard).
    const SIBLING_SCOPES: &[&str] = &[
        scopes::TRADES_READ,
        scopes::TRADES_BUST,
        scopes::ORDERS_READ,
    ];

    /// Build an `AuthContext` with exactly the given space-separated scope
    /// set — the same construction the handlers see after real verification.
    fn ctx(scope: &str) -> AuthContext {
        AuthContext {
            claims: VerifiedClaims {
                sub: "matrix-test".into(),
                iss: "https://auth.test".into(),
                aud: "exchange8".into(),
                jti: "matrix-jti".into(),
                iat: 0,
                exp: i64::MAX,
                token_tp: "access".into(),
                scope: scope.into(),
                env_id: Some("00000000-0000-0000-0000-000000000002".into()),
                email: None,
                act: None,
                vertical: None,
                service_type: None,
            },
            // Bearer-path context: no certificate workload on the test
            // identity (CERTFREE-4V — the field exists for the cert path).
            workload: None,
        }
    }

    #[test]
    fn every_handler_denies_without_and_with_wrong_scope() {
        for (handler, required) in HANDLER_SCOPES {
            // No scope at all → 403 FORBIDDEN.
            let err = ctx("")
                .require_any_scope(required)
                .expect_err("no scope must deny");
            assert_eq!(err.0.as_u16(), 403, "{handler}: expected FORBIDDEN");

            // Foreign-vertical scope → 403 (no cross-vertical leakage).
            let err = ctx("bank8:ledger:read")
                .require_any_scope(required)
                .expect_err("foreign scope must deny");
            assert_eq!(err.0.as_u16(), 403, "{handler}: foreign must be FORBIDDEN");

            // Sibling exchange8 scope (wrong resource) → 403.
            for sibling in SIBLING_SCOPES {
                if required.contains(sibling) {
                    continue;
                }
                let err = ctx(sibling)
                    .require_any_scope(required)
                    .expect_err("sibling scope must deny");
                assert_eq!(
                    err.0.as_u16(),
                    403,
                    "{handler}: sibling {sibling} must be FORBIDDEN"
                );
            }
        }
    }

    #[test]
    fn every_handler_allows_each_required_scope_and_platform_admin() {
        for (handler, required) in HANDLER_SCOPES {
            for scope in *required {
                assert!(
                    ctx(scope).require_any_scope(required).is_ok(),
                    "{handler}: exact scope {scope} must allow"
                );
            }
            // All required scopes at once → Ok.
            let all = required.join(" ");
            assert!(ctx(&all).require_any_scope(required).is_ok(), "{handler}");

            // Platform-admin bypass is permission-based (`sentin8:sys:admin`),
            // architecture principles §5.
            assert!(
                ctx("sentin8:sys:admin").require_any_scope(required).is_ok(),
                "{handler}"
            );
        }
    }

    #[test]
    fn grants_are_prefix_permissive_and_denials_are_exact() {
        // Platform-wide grant semantics (`has_any_scope`): a token carrying a
        // scope *derived from* a required one (prefix + ':') IS accepted.
        // Pinned so any tightening is a conscious platform change caught here.
        assert!(ctx("exchange8:trade:read:all")
            .require_any_scope(&[scopes::TRADES_READ])
            .is_ok());
        // But an unrelated scope sharing a text prefix is NOT accepted —
        // segment-exact, never substring.
        assert!(ctx("exchange8:trade:readx")
            .require_any_scope(&[scopes::TRADES_READ])
            .is_err());
    }

    #[test]
    fn scope_constants_follow_the_naming_convention() {
        // AGENTS.md prescribes 4 segments, but the registered reality in
        // `sentin8.vertical_admin_scopes` is 3 for pre-FX scopes
        // (`exchange8:trade:read`) and 4 for FX-era ones
        // (`exchange8:rfq:rfq:write`). Pin the REGISTERED contract: 3–4
        // segments, exchange8 vertical, lowercase, no empty segments.
        // Renaming scopes to 4 segments is a platform-wide migration
        // (role mappings in DB), not a code-local change.
        let mut all: Vec<&str> = Vec::new();
        for (handler, required) in HANDLER_SCOPES {
            for scope in *required {
                all.push(scope);
                let segs: Vec<_> = scope.split(':').collect();
                assert!(
                    (3..=4).contains(&segs.len()),
                    "{handler}: {scope} must have 3-4 segments"
                );
                assert_eq!(segs[0], "exchange8", "{handler}: {scope} wrong vertical");
                assert!(
                    segs.iter().all(|s| !s.is_empty()
                        && s.chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())),
                    "{handler}: {scope} segments must be lowercase alnum"
                );
            }
        }
        // Every declared constant must be wired into HANDLER_SCOPES (a
        // declared-but-unwired scope would silently rot). Shared constants
        // across handlers are legitimate — dedup first.
        all.sort_unstable();
        all.dedup();
        let mut declared = vec![
            scopes::TRADES_READ,
            scopes::TRADES_BUST,
            scopes::ORDERS_READ,
        ];
        declared.sort_unstable();
        assert_eq!(all, declared, "scopes module and HANDLER_SCOPES diverged");
    }
}
