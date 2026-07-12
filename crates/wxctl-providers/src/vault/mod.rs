//! `vault` (HashiCorp Vault) service handlers.
//!
//! `vault_policy` needs a custom handler (PolicyHandler) to read `policy_file` into the
//! `policy` body and to unwrap Vault's `data` response envelope on discovery.
//!
//! Phase 2 identity/auth kinds: `vault_auth_method` (AuthMethodHandler does the config
//! sub-write plus accessor read-back) and `vault_identity_group` (IdentityGroupHandler
//! hoists the canonical `id`); `vault_jwt_role` and `vault_group_alias` need only the
//! `data`-unwrap, so they register to the shared generic EnvelopeHandler.
//!
//! Secrets-engine kinds (Phase 3): `vault_secret_engine` (SecretEngineHandler does the
//! database mount enable plus connection-config sub-write with sensitive `password`
//! redaction), plus `vault_database_role` and `vault_audit_device`, which need only the
//! `data`-unwrap and register to the shared generic EnvelopeHandler.

pub mod handlers;

define_handlers! {
    "vault_policy" => handlers::PolicyHandler,
    "vault_auth_method" => handlers::AuthMethodHandler,
    "vault_jwt_role" => handlers::EnvelopeHandler,
    "vault_identity_group" => handlers::IdentityGroupHandler,
    "vault_group_alias" => handlers::GroupAliasHandler,
    "vault_secret_engine" => handlers::SecretEngineHandler,
    "vault_database_role" => handlers::EnvelopeHandler,
    "vault_audit_device" => handlers::EnvelopeHandler,
}
