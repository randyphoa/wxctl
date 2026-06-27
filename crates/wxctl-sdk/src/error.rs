use thiserror::Error;

/// SDK error type. The engine returns `anyhow::Error`, so most failures arrive
/// through `Other`; `HttpError`/`IoError` carry typed transport/IO sources when a
/// `reqwest`/`io` error is propagated directly.
#[derive(Debug, Error)]
pub enum WxctlError {
    #[error("HTTP request failed: {0}")]
    HttpError(#[from] reqwest::Error),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, WxctlError>;
