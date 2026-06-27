pub mod handlers;

define_handlers! {
    "milvus_service" => handlers::MilvusServiceHandler,
    "prestissimo_engine" => handlers::PrestissimoEngineHandler,
    "presto_engine" => handlers::PrestoEngineHandler,
    "spark_engine" => handlers::SparkEngineHandler,
    "ingestion_job" => handlers::IngestionJobHandler,
    "sal_enrichment_job" => handlers::SalEnrichmentJobHandler,
    "sal_glossary" => handlers::SalGlossaryHandler,
    "storage_registration" => handlers::StorageRegistrationHandler,
    "database_registration" => handlers::DatabaseRegistrationHandler,
    "database_connection" => handlers::DatabaseConnectionHandler,
    "schema" => handlers::SchemaHandler,
}
