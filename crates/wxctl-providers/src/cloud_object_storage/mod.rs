//! Cloud Object Storage provider — S3-family buckets + connections, plus
//! register-only ADLS / GCS bucket kinds.
//!
//! `storage_connection` carries creds, `s3_bucket` / `s3_object` front the
//! S3 REST API. `adls_container` / `gcs_bucket` are register-only: they
//! validate the linked connection family and expose passthrough state for
//! `storage_registration` without calling Azure / GCS.

pub mod common;
pub mod cos_client;
pub mod handlers;

define_handlers! {
    "storage_connection" => handlers::StorageConnectionHandler,
    "s3_bucket" => handlers::S3BucketHandler,
    "s3_object" => handlers::S3ObjectHandler,
    "adls_container" => handlers::AdlsContainerHandler,
    "gcs_bucket" => handlers::GcsBucketHandler,
}
