use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConcurrencyConfig {
    #[serde(default = "default_global_limit")]
    pub global_limit: usize,

    #[serde(default)]
    pub service_limits: HashMap<String, usize>,

    /// Timeout for each operation in the executor (includes retries, polling loops, multiple HTTP calls)
    #[serde(default = "default_operation_timeout")]
    pub default_timeout_secs: u64,

    /// Timeout for each individual HTTP request
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
}

fn default_global_limit() -> usize {
    50
}

fn default_operation_timeout() -> u64 {
    900
}

fn default_request_timeout() -> u64 {
    30
}

impl Default for ConcurrencyConfig {
    fn default() -> Self {
        Self {
            global_limit: default_global_limit(),
            service_limits: HashMap::from([("wkc".to_string(), 10), ("catalog".to_string(), 20), ("governance".to_string(), 10), ("instana".to_string(), 10), ("planning_analytics".to_string(), 10)]),
            default_timeout_secs: default_operation_timeout(),
            request_timeout_secs: default_request_timeout(),
        }
    }
}

impl ConcurrencyConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(val) = std::env::var("WXCTL_CONCURRENCY_GLOBAL")
            && let Ok(limit) = val.parse::<usize>()
        {
            config.global_limit = limit.max(1);
        }

        if let Ok(val) = std::env::var("WXCTL_CONCURRENCY_TIMEOUT")
            && let Ok(timeout) = val.parse()
        {
            config.default_timeout_secs = timeout;
        }

        if let Ok(val) = std::env::var("WXCTL_REQUEST_TIMEOUT")
            && let Ok(timeout) = val.parse()
        {
            config.request_timeout_secs = timeout;
        }

        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_clamps_and_preserves_global_limit() {
        let _env = crate::test_env_lock();
        // "0" clamps to the floor of 1; a valid value passes through unchanged.
        for (raw, expected) in [("0", 1usize), ("25", 25)] {
            unsafe { std::env::set_var("WXCTL_CONCURRENCY_GLOBAL", raw) };
            let config = ConcurrencyConfig::from_env();
            assert_eq!(config.global_limit, expected, "raw={raw}");
        }
        unsafe { std::env::remove_var("WXCTL_CONCURRENCY_GLOBAL") };
    }
}
