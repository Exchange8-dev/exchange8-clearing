// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Exchange8:Clearing
// SPDX-License-Identifier: Apache-2.0

//! Exchange8:CLEARING service binary - event-sourced order/trade ledger + drop copy.
//!
//! Design: `design/sentin8-exchange8-clearing-technical-design.md` (see the
//! service doc for the exact file). Core logic in the sibling modules;
//! this shell wires the tokio HTTP/TLS surface via the platform
//! `sentin8-shell` crate (registration, DB pool, health).

#![allow(dead_code)]

// (master design §11.1). Workers (bus consumers, session
// supervisors, purge jobs) are in-process tokio tasks, so the
mod drop_copy;
mod handlers;
mod locate;
mod openapi;
mod projector;
mod registration;

fn extend_router(
    app: axum::Router<std::sync::Arc<sentin8::shell::AppState>>,
    _state: std::sync::Arc<sentin8::shell::AppState>,
) -> axum::Router<std::sync::Arc<sentin8::shell::AppState>> {
    // ── Sentin8:Policy admin API (policy design §3.6 - shared implementation;
    // the shell AppState already implements HasPolicyAdmin).
    sentin8::shell::mount_policy_admin(
        // Projectors run on the Bus consumer task (design §4.1). The HTTP
        // router exposes the ledger query API (trades/orders/bust - RLS-scoped)
        // + health; admin/RFW data endpoints alias the same handlers.
        app.route("/v1/trades", axum::routing::get(handlers::list_trades))
            .route("/v1/trades/bust", axum::routing::post(handlers::bust_trade))
            .route("/v1/orders", axum::routing::get(handlers::list_orders))
            .route(
                "/v1/admin/trades",
                axum::routing::get(handlers::list_trades),
            )
            .route(
                "/v1/admin/orders",
                axum::routing::get(handlers::list_orders),
            ),
    )
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sentin8::shell::serve(
        sentin8::shell::Sentin8Shell::new("exchange8:clearing", "EXCHANGE8_CLEARING")
            .with_registration(registration::print)
            .with_simple_state()
            .with_router(extend_router),
    )
    .await
}
