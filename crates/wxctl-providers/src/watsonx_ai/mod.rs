pub mod handlers;

define_handlers! {
    "ai_service" => handlers::AiServiceHandler,
    "autoai_experiment" => handlers::AutoaiExperimentHandler,
    "notebook" => handlers::NotebookHandler,
    "wml_deployment" => handlers::WmlDeploymentHandler,
    "wml_function" => handlers::WmlFunctionHandler,
    "wml_model" => handlers::WmlModelHandler,
    // wml_script shares the WML functions API (see wml_script.yaml); same handler, distinct kind.
    "wml_script" => handlers::WmlFunctionHandler,
}
