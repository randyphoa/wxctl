use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;
use wxctl_core::client::HttpClient;
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

pub struct PythonScriptHandler;

impl ResourceHandler for PythonScriptHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { execute_python(resource, operation_id).await })
    }

    fn pre_update<'a>(&'a self, _current: &'a Value, resource: &'a mut Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { execute_python(resource, operation_id).await })
    }
}

async fn execute_python<'a>(resource: &'a mut Value, operation_id: &'a str) -> Result<HookOutcome> {
    // ref_name may be filtered out by LocalOnly - use a fallback
    let name = resource.get("ref_name").or_else(|| resource.get("name")).and_then(|v| v.as_str()).unwrap_or("python_script");

    // Get script content - either inline or from file
    let script = match (resource.get("script"), resource.get("script_path")) {
        (Some(Value::String(s)), _) => s.clone(),
        (_, Some(Value::String(p))) => {
            let path = PathBuf::from(p);
            if !path.exists() {
                bail!("Script file not found: {}", path.display());
            }
            tokio::fs::read_to_string(&path).await?
        }
        _ => bail!("Either 'script' or 'script_path' is required"),
    };

    // Get configuration
    let python_path = resource.get("python_path").and_then(|v| v.as_str()).unwrap_or("python3");

    let timeout_secs = resource.get("timeout_seconds").and_then(|v| v.as_u64()).unwrap_or(300);

    let working_dir = resource.get("working_dir").and_then(|v| v.as_str()).map(PathBuf::from);

    // Build command
    let mut cmd = tokio::process::Command::new(python_path);
    cmd.arg("-c").arg(&script);

    // Add arguments
    if let Some(args) = resource.get("args").and_then(|v| v.as_array()) {
        for arg in args {
            if let Some(s) = arg.as_str() {
                cmd.arg(s);
            }
        }
    }

    // Set environment variables
    if let Some(env) = resource.get("env").and_then(|v| v.as_object()) {
        for (key, value) in env {
            if let Some(v) = value.as_str() {
                cmd.env(key, v);
            }
        }
    }

    // Set working directory
    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    tracing::info!(
        target: "wxctl::substage::provider",
        operation_id = %operation_id,
        name = %name,
        "Executing Python script"
    );

    // Execute with timeout
    let output = tokio::time::timeout(Duration::from_secs(timeout_secs), cmd.output()).await.map_err(|_| anyhow!("Script execution timed out after {}s", timeout_secs))?.map_err(|e| anyhow!("Failed to execute Python: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);

    if !output.status.success() {
        tracing::warn!(
            target: "wxctl::substage::provider",
            operation_id = %operation_id,
            name = %name,
            exit_code = %exit_code,
            stderr = %stderr,
            "Python script failed"
        );
    } else {
        tracing::info!(
            target: "wxctl::substage::provider",
            operation_id = %operation_id,
            name = %name,
            "Python script completed successfully"
        );
    }

    // Return result - HookOutcome::Handled skips HTTP call
    Ok(HookOutcome::Handled(json!({
        "name": name,
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": stderr,
        "success": output.status.success()
    })))
}
