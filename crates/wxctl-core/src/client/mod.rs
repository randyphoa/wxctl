pub mod factory;
pub mod http;
pub mod materializer;
mod multipart;
pub mod request;
mod retry;
pub mod token;

pub use factory::{ClientFactory, load_color_preference};
pub use http::{HttpClient, error_has_status, error_matches, join_url};
pub use materializer::{BodyKindSelector, RequestMaterializer, extract_nested};
pub use request::{BodyKind, RequestSpec};
pub use token::TokenManager;

// Re-export reqwest::Method for handlers that need to create custom requests
pub use reqwest::Method;
