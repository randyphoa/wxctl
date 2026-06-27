pub mod flow;
pub mod handlers;
pub mod mcp;
pub mod openapi;
pub mod python;

define_handlers! {
    "orchestrate_connection" => handlers::ConnectionHandler,
    "knowledge_base" => handlers::KnowledgeBaseHandler,
    "tool" => handlers::ToolHandler,
    "toolkit" => handlers::ToolkitHandler,
    "agent" => handlers::AgentHandler,
    "model" => handlers::ModelHandler,
}
