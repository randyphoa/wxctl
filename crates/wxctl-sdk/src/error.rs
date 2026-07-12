use thiserror::Error;

/// SDK error type. The engine and providers return `anyhow::Error`, so every failure
/// arrives through `Other`.
#[derive(Debug, Error)]
pub enum WxctlError {
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, WxctlError>;
