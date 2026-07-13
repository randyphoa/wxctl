#[derive(Debug)]
pub(super) enum ExecutionError {
    Timeout,
    Network(String),
    Server { status: u16, message: String },
    Client { status: u16, message: String },
    Internal(String),
    Cancelled,
}

impl ExecutionError {
    pub(super) fn message(&self) -> String {
        match self {
            Self::Timeout => "Operation timed out".to_string(),
            Self::Network(msg) => format!("Network error: {}", msg),
            Self::Server { status, message } => format!("Server error {}: {}", status, message),
            Self::Client { status, message } => format!("Client error {}: {}", status, message),
            Self::Internal(msg) => format!("Internal error: {}", msg),
            Self::Cancelled => "Operation cancelled".to_string(),
        }
    }

    pub(super) fn from_reqwest_error(e: &reqwest::Error) -> Self {
        if e.is_timeout() {
            Self::Timeout
        } else if e.is_connect() || e.is_request() {
            Self::Network(e.to_string())
        } else if let Some(status) = e.status() {
            let code = status.as_u16();
            if status.is_server_error() || code == 429 { Self::Server { status: code, message: e.to_string() } } else { Self::Client { status: code, message: e.to_string() } }
        } else {
            Self::Internal(e.to_string())
        }
    }

    pub(super) fn from_anyhow(e: &anyhow::Error) -> Self {
        if let Some(reqwest_err) = e.downcast_ref::<reqwest::Error>() {
            return Self::from_reqwest_error(reqwest_err);
        }
        if e.downcast_ref::<tokio::time::error::Elapsed>().is_some() {
            return Self::Timeout;
        }
        if let Some(io_err) = e.downcast_ref::<std::io::Error>() {
            return Self::Network(io_err.to_string());
        }
        // Use full error chain ({:#}) to preserve context from .context() wrappers
        Self::Internal(format!("{:#}", e))
    }
}

impl std::fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message())
    }
}
