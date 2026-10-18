// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Exchange8:Clearing
// SPDX-License-Identifier: Apache-2.0

//! OpenAPI spec for Exchange8:Clearing.
//!
//! utoipa annotations on the service's handlers + the shared Shield
//! policy-admin surface (`sentin8::shell::policy_*` wrappers); the
//! shell-owned `/v1/health` mount is described through a spec wrapper
//! (`sentin8/design/sentin8-openapi-coverage-design.md` §2).

use utoipa::OpenApi;

pub mod w {

    /// Spec wrapper for the shell-owned `/v1/health` mount.
    #[utoipa::path(
        get,
        path = "/v1/health",
        responses(
            (status = 200, description = "Service is healthy", body = serde_json::Value),
        ),
        security(),
        tag = "clearing",
    )]
    pub async fn clearing_health() {}
    /// Spec wrapper for the alias route (same handler as the public path).
    #[utoipa::path(
        get,
        path = "/v1/admin/trades",
        responses(
            (status = 200, description = "OK", body = serde_json::Value),
            (status = 403, description = "Forbidden"),
        ),
        security(),
        tag = "exchange8-admin",
    )]
    pub async fn admin_list_trades() {}

    /// Spec wrapper for the alias route (same handler as the public path).
    #[utoipa::path(
        get,
        path = "/v1/admin/orders",
        responses(
            (status = 200, description = "OK", body = serde_json::Value),
            (status = 403, description = "Forbidden"),
        ),
        security(),
        tag = "exchange8-admin",
    )]
    pub async fn admin_list_orders() {}
}

/// OpenAPI spec for the clearing API.
#[derive(OpenApi)]
#[openapi(
    paths(
        crate::openapi::w::admin_list_trades,
        crate::openapi::w::admin_list_orders,
crate::handlers::list_trades,
crate::handlers::bust_trade,
crate::handlers::list_orders,
crate::openapi::w::clearing_health,
        sentin8::shell::policy_list_sets,
        sentin8::shell::policy_get_set,
        sentin8::shell::policy_put_set,
        sentin8::shell::policy_delete_set,
        sentin8::shell::policy_test_set,
        sentin8::shell::policy_mode_enforce,
        sentin8::shell::policy_mode_shadow,
        sentin8::shell::policy_mode_disable,
        sentin8::shell::policy_create_set,
        sentin8::shell::policy_save_set,
        sentin8::shell::policy_flip_set,
        sentin8::shell::policy_list_decisions,
        sentin8::shell::policy_delete_relation_path,
        sentin8::shell::policy_delete_attribute_path,
        sentin8::shell::policy_list_relations,
        sentin8::shell::policy_put_relation,
        sentin8::shell::policy_delete_relation,
        sentin8::shell::policy_check_relation,
        sentin8::shell::policy_list_attributes,
        sentin8::shell::policy_put_attribute,
        sentin8::shell::policy_delete_attribute,
    ),
    components(schemas()),
    security(),
    tags(
        (name = "clearing", description = "Exchange8:Clearing"),
        (name = "exchange8-admin", description = "Exchange8 admin surface (RFW)"),
        (name = "policy", description = "Shield policy administration (shared surface)"),
    ),
)]
pub struct ClearingOpenApi;
