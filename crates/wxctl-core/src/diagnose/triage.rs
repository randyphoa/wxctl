//! Per-error triage. Maps an error code (+ stage) to one of three classes, each
//! carrying agent-facing guidance. Codes are the `WXCTL-{LETTER}{NUM}` strings from
//! `crate::logging::error_codes`; the panic code `WXCTL-P001` and the top-level
//! `WXCTL-E000` chain code are emitted by the binary's panic/error hooks.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TriageClass {
    /// Retry-able: 5xx, connection/timeout, rate limit. Guidance points at the
    /// concurrency/timeout env knobs.
    Transient,
    /// User-fixable in the config or environment: validation, profile, auth.
    ConfigEnv,
    /// Likely a wxctl source bug: parse failures, panics, invariant violations.
    /// Guidance points at the `src`/span path and, when not full-trace, at
    /// re-running with `--full-trace`.
    SuspectedWxctlBug,
}

impl TriageClass {
    pub fn label(self) -> &'static str {
        match self {
            TriageClass::Transient => "transient",
            TriageClass::ConfigEnv => "config/env",
            TriageClass::SuspectedWxctlBug => "suspected wxctl bug",
        }
    }

    /// One-line, action-oriented guidance for this class.
    pub fn guidance(self) -> &'static str {
        match self {
            TriageClass::Transient => "Transient backend/transport failure. Retry the command. If it persists, lower concurrency (WXCTL_CONCURRENCY_GLOBAL) or raise timeouts (WXCTL_CONCURRENCY_TIMEOUT, WXCTL_REQUEST_TIMEOUT).",
            TriageClass::ConfigEnv => "Fix in the config or environment: correct the indicated field, set the missing ${env:} variable, or check the profile's URL/credentials.",
            TriageClass::SuspectedWxctlBug => "Likely a wxctl source bug. Use the src location + span path below to localize it. If this run was not --full-trace, re-run with --full-trace to capture hook payload diffs, the full error_chain, and any backtrace.",
        }
    }
}

/// Classify by error code.
pub fn classify(error_code: &str) -> TriageClass {
    match error_code {
        // Transient — retry-able.
        "WXCTL-H002" | "WXCTL-H003" | "WXCTL-E005" => TriageClass::Transient,
        // Handler not implemented — deferred-handler stub, not a user config error.
        "WXCTL-H900" => TriageClass::SuspectedWxctlBug,
        // Suspected wxctl bug — panics + the top-level anyhow chain code.
        "WXCTL-P001" => TriageClass::SuspectedWxctlBug,
        // Everything else: split by the stage-letter prefix.
        code => {
            // WXCTL-{LETTER}{...}
            let letter = code.strip_prefix("WXCTL-").and_then(|s| s.chars().next());
            match letter {
                // Validation, configuration, template → config/env (user-fixable).
                Some('V') | Some('C') | Some('T') => TriageClass::ConfigEnv,
                // Reconciliation, execution, HTTP → config/env by default
                // (404/409/auth are the common, fixable cases). The transient
                // subset (H002/H003/E005) is handled explicitly above.
                // H900 (deferred handler) is also handled explicitly above.
                Some('R') | Some('E') | Some('H') => TriageClass::ConfigEnv,
                // Unknown / parse failures with no code →
                // suspected wxctl bug.
                _ => TriageClass::SuspectedWxctlBug,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_buckets_codes_into_triage_classes() {
        let cases = [
            // Transient: server/retryable failures.
            ("WXCTL-H002", TriageClass::Transient),
            ("WXCTL-H003", TriageClass::Transient),
            ("WXCTL-E005", TriageClass::Transient),
            // ConfigEnv: user-fixable validation/config/auth/client (V/C/T/R/E/H prefixes).
            ("WXCTL-V003", TriageClass::ConfigEnv),
            ("WXCTL-V301", TriageClass::ConfigEnv),
            ("WXCTL-C001", TriageClass::ConfigEnv),
            ("WXCTL-H004", TriageClass::ConfigEnv),
            ("WXCTL-H001", TriageClass::ConfigEnv),
            // SuspectedWxctlBug: panics, unknown/no-prefix codes, and the H900 deferred-handler stub.
            ("WXCTL-P001", TriageClass::SuspectedWxctlBug),
            ("UNKNOWN", TriageClass::SuspectedWxctlBug),
            ("WXCTL-Z999", TriageClass::SuspectedWxctlBug),
            ("WXCTL-H900", TriageClass::SuspectedWxctlBug),
        ];
        for (code, expected) in cases {
            assert_eq!(classify(code), expected, "code={code}");
        }
    }
}
