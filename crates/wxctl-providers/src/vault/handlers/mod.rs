//! Custom `vault` (HashiCorp Vault) resource handlers.
//!
//! `envelope` — shared `data`-object unwrap reused by every vault handler's
//! `post_discover` (Vault wraps read payloads in a top-level `data` object), plus the
//! generic `EnvelopeHandler` for kinds whose only custom behavior is that unwrap.
//!
//! `policy` — `vault_policy`: reads `policy_file` into the `policy` body field in
//! `pre_create`/`pre_update`; unwraps the `data` envelope on discovery.
//!
//! `auth_method` — `vault_auth_method`: JWT-config sub-write + accessor read-back in
//! `post_create`.
//!
//! `identity_group` — `vault_identity_group`: hoists the computed canonical `id` from
//! `data.id` on the create response.
//!
//! `group_alias` — `vault_group_alias`: discovers via the parent group's embedded `alias`
//! (Vault has no alias-by-name lookup) and lifts `data.alias` to the top level.

mod auth_method;
pub(crate) mod envelope;
mod group_alias;
mod identity_group;
mod policy;
mod secret_engine;

pub use auth_method::AuthMethodHandler;
pub use envelope::EnvelopeHandler;
pub use group_alias::GroupAliasHandler;
pub use identity_group::IdentityGroupHandler;
pub use policy::PolicyHandler;
pub use secret_engine::SecretEngineHandler;
