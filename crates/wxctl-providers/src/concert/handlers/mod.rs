//! Custom `concert` resource handlers.
//!
//! `application` — Concert's create/GET responses never carry `application_release_id`
//! flat (only nested at `associations.releases[0].id`), so `ApplicationHandler` hoists it
//! to top level in both `post_create` and `post_discover`, so
//! `${concert_application.<ref>.application_release_id}` resolves on first-apply and on
//! re-apply/replan against an already-discovered application alike.
//!
//! `source_repo` — Concert's bulk `POST /source_repos` create wraps repos in a
//! `source_repos` array (not a declared schema field), so `SourceRepoHandler`
//! owns the POST via `HookOutcome::Handled`.
//!
//! `credential` — Concert has no item DELETE for credentials; deletion is a
//! collection op (`DELETE /credentials?delete_ids={id}`), so `CredentialHandler`
//! owns the delete via `pre_delete` returning `HookOutcome::Handled`.
//!
//! `automation_rule` — same collection-delete shape as `credential`
//! (`DELETE /automation_rules?delete_ids={id}`), owned by `AutomationRuleHandler`.
//!
//! `compliance_profile` — POST /compliance/api/v1/profiles returns {message} with no id, so
//! ComplianceProfileHandler recovers the uuid by listing /profiles and matching title
//! (post_create + recover_from_create_error).
//!
//! `resilience_library` — Concert's resilience library create returns `library_id`
//! while read/list return `id`, so `ResilienceLibraryHandler` maps `library_id → id`
//! in `post_create` (and recovers an existing library by name on a create conflict).
//!
//! `resilience_profile` — Concert's resilience profile create returns `profile_id` while
//! read/list return `id`, so `ResilienceProfileHandler` maps `profile_id -> id` in
//! `post_create` (and recovers an existing profile by name on a create conflict). The API
//! has no profile update verb, so drift reconciles by Recreate via `immutable_fields`.
//!
//! `resilience_posture` — Concert's resilience posture create returns `posture_id` while
//! read/list return `id`, so `ResiliencePostureHandler` maps `posture_id -> id` in
//! `post_create` (and recovers an existing posture by name on a create conflict). It binds
//! its profile by name and exposes only a narrow PATCH (assessment_period + comments); every
//! other writable field is immutable (drift -> Recreate).

mod application;
mod automation_rule;
mod common;
mod compliance_profile;
mod credential;
mod library;
mod resilience_posture;
mod resilience_profile;
mod source_repo;

pub use application::ApplicationHandler;
pub use automation_rule::AutomationRuleHandler;
pub use compliance_profile::ComplianceProfileHandler;
pub use credential::CredentialHandler;
pub use library::ResilienceLibraryHandler;
pub use resilience_posture::ResiliencePostureHandler;
pub use resilience_profile::ResilienceProfileHandler;
pub use source_repo::SourceRepoHandler;
