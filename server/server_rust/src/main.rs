// Rust guideline compliant 2026-02-21

use anyhow::{Context, Result};
use axum::{
    http::{HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{Html, Response},
    routing::{get, post},
    Json, Router,
};
use base64::prelude::*;
use mimalloc::MiMalloc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Debug;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::process::Command;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// Payload accepted by the POST /run endpoint.
#[derive(Debug, Deserialize, Serialize)]
pub struct RunRequest {
    /// The Python source code to execute.
    pub code: String,
    /// Egress network mode: offline, isolated, full.
    pub network: Option<String>,
    /// CPU core limit.
    pub cpus: Option<f64>,
    /// Memory limit in MB.
    pub memory_mb: Option<i64>,
}

/// Sandbox execution resource metrics.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Metrics {
    /// Wall-clock time of the container execution on the host in milliseconds.
    pub wall_time_ms: i64,
    /// Maximum RSS memory used by the sandbox in Kilobytes.
    pub max_memory_kb: i64,
    /// CPU percentage of the execution run.
    pub cpu_percentage: String,
    /// User CPU time spent in seconds.
    pub user_time_sec: f64,
    /// System CPU time spent in seconds.
    pub sys_time_sec: f64,
    /// Number of file system read operations.
    pub fs_inputs: i64,
    /// Number of file system write operations.
    pub fs_outputs: i64,
    /// Voluntary context switches.
    pub voluntary_context_switches: i64,
    /// Involuntary context switches.
    pub involuntary_context_switches: i64,
}

/// Execution results returned by the POST /run endpoint.
#[derive(Debug, Deserialize, Serialize)]
pub struct RunResponse {
    /// Standard output of the sandbox process.
    pub stdout: String,
    /// Standard error of the sandbox process.
    pub stderr: String,
    /// Exit code of the container run.
    pub exit_code: i32,
    /// Sandbox resource metrics.
    pub metrics: Metrics,
    /// Directory map of output filenames to base64-encoded file contents.
    pub output_files: HashMap<String, String>,
}

/// Launches the Axum web service.
///
/// Starts an asynchronous HTTP server on port 8080 or port specified in PORT env var.
///
/// # Errors
/// Returns an error if the server fails to bind or start.
async fn cors_middleware(request: axum::extract::Request, next: Next) -> Response {
    let method = request.method().clone();
    if method == Method::OPTIONS {
        let mut response = Response::default();
        let headers = response.headers_mut();
        headers.insert("access-control-allow-origin", HeaderValue::from_static("*"));
        headers.insert("access-control-allow-methods", HeaderValue::from_static("GET, POST, OPTIONS"));
        headers.insert("access-control-allow-headers", HeaderValue::from_static("content-type, authorization"));
        return response;
    }

    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("access-control-allow-origin", HeaderValue::from_static("*"));
    headers.insert("access-control-allow-methods", HeaderValue::from_static("GET, POST, OPTIONS"));
    headers.insert("access-control-allow-headers", HeaderValue::from_static("content-type, authorization"));
    response
}

#[tokio::main]
pub async fn main() -> Result<()> {
    let app = Router::new()
        .route("/", get(handle_index))
        .route("/run", post(handle_run))
        .route("/libraries", get(handle_libraries))
        .route("/tiff.min.js", get(handle_tiff))
        .layer(axum::middleware::from_fn(cors_middleware));

    let port = std::env::var("PORT").unwrap_or_else(|_| "8081".to_string());
    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind to {}", addr))?;

    println!("[Rust Worker Service] Listening on: {}", addr);
    axum::serve(listener, app)
        .await
        .context("Server failed during execution")?;

    Ok(())
}

/// HTTP handler that returns a list of pre-installed Python libraries in the sandbox.
///
/// Queries the sandbox image for distributions and merges with a built-in Python module list.
///
/// # Errors
/// Returns an internal server error status code if the podman process command fails.
async fn handle_libraries() -> Result<Json<Vec<String>>, StatusCode> {
    let output = Command::new("podman")
        .args([
            "run", "--rm", "run-python-sandbox", "python3", "-c",
            "import importlib.metadata, json; print(json.dumps([d.metadata['Name'] for d in importlib.metadata.distributions()]))"
        ])
        .output()
        .await;

    let mut libs = Vec::new();
    if let Ok(out) = output {
        if let Ok(stdout) = String::from_utf8(out.stdout) {
            if let Ok(parsed) = serde_json::from_str::<Vec<String>>(&stdout) {
                libs = parsed;
            }
        }
    }

    let builtins = vec![
        "os".to_string(), "sys".to_string(), "json".to_string(),
        "math".to_string(), "urllib".to_string(), "time".to_string(),
        "subprocess".to_string(), "random".to_string(), "re".to_string()
    ];

    let mut seen = std::collections::HashSet::new();
    let mut final_libs = Vec::new();

    for b in builtins {
        seen.insert(b.clone());
        final_libs.push(b);
    }

    let mut package_map = HashMap::new();
    package_map.insert("Pillow".to_string(), "PIL".to_string());
    package_map.insert("reportlab".to_string(), "reportlab".to_string());
    package_map.insert("pypdf".to_string(), "pypdf".to_string());

    for l in libs {
        let import_name = package_map.get(&l).cloned().unwrap_or(l);
        if !seen.contains(&import_name) {
            seen.insert(import_name.clone());
            final_libs.push(import_name);
        }
    }

    Ok(Json(final_libs))
}

/// HTTP handler that serves the Monaco code editor web interface.
///
/// Reads index.html from multiple search paths and returns its content.
///
/// # Errors
/// Returns an internal server error status code if index.html cannot be located.
async fn handle_index() -> Result<Html<String>, StatusCode> {
    let paths = ["wfe/index.html", "../wfe/index.html", "../../wfe/index.html", "index.html"];
    for p in &paths {
        if let Ok(content) = fs::read_to_string(p).await {
            return Ok(Html(content));
        }
    }
    eprintln!("[Rust Worker] Error: index.html not found.");
    Err(StatusCode::INTERNAL_SERVER_ERROR)
}

/// HTTP handler that serves the local tiff.min.js script.
async fn handle_tiff() -> Result<(axum::http::HeaderMap, String), StatusCode> {
    let paths = ["wfe/tiff.min.js", "../wfe/tiff.min.js", "../../wfe/tiff.min.js", "tiff.min.js"];
    for p in &paths {
        if let Ok(content) = fs::read_to_string(p).await {
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/javascript"),
            );
            return Ok((headers, content));
        }
    }
    eprintln!("[Rust Worker] Error: tiff.min.js not found.");
    Err(StatusCode::NOT_FOUND)
}

/// HTTP handler that executes Python code inside the sandbox container.
///
/// Extracts parameters, writes temp files, runs Podman asynchronously, and maps outputs.
///
/// # Errors
/// Returns an internal server error status code if sandbox file system operations fail.
async fn handle_run(Json(payload): Json<RunRequest>) -> Result<Json<RunResponse>, StatusCode> {
    let network = payload.network.unwrap_or_else(|| "offline".to_string());
    if network != "offline" && network != "isolated" && network != "full" {
        return Err(StatusCode::BAD_REQUEST);
    }

    match execute_sandbox(&payload.code, &network, payload.cpus, payload.memory_mb).await {
        Ok(response) => Ok(Json(response)),
        Err(e) => {
            eprintln!("[Rust Worker] Execution error: {:?}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Generates a unique execution run ID based on system timestamp.
///
/// # Panics
/// Panics if the system time goes backwards before UNIX_EPOCH.
fn generate_run_id() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("System time must be after UNIX EPOCH")
        .as_nanos()
}

/// Performs filesystem preparation, asynchronous Podman invocation, and result ingestion.
///
/// # Errors
/// Returns an error if any system command, file creation, or file read fails.
async fn execute_sandbox(code: &str, network: &str, cpus: Option<f64>, memory_mb: Option<i64>) -> Result<RunResponse> {
    let run_id = generate_run_id();
    let temp_dir = std::env::temp_dir();

    // 1. Establish isolated paths for the execution run
    let run_dir = temp_dir.join(format!("sandbox-run-rust-{}", run_id));
    let out_dir = temp_dir.join(format!("sandbox-out-rust-{}", run_id));

    fs::create_dir_all(&run_dir)
        .await
        .context("Failed to create execution run directory")?;
    fs::create_dir_all(&out_dir)
        .await
        .context("Failed to create output directory")?;

    // We must ensure the output folder is writable by the container's unprivileged UID
    set_directory_writable(&out_dir).await?;

    let py_file_path = run_dir.join("run.py");
    fs::write(&py_file_path, code)
        .await
        .context("Failed to write python script to run_dir")?;

    // 2. Prepare the rootless Podman execution parameters
    let py_mount = format!("{}:/sandbox/run.py:ro", py_file_path.display());
    let out_mount = format!("{}:/output:rw", out_dir.display());

    println!("[Rust Worker] Spawning container with network={}...", network);
    let start_time = std::time::Instant::now();

    let mut podman_args = vec![
        "run".to_string(), "--rm".to_string(),
        "--cap-add=NET_ADMIN".to_string(),
        "--cap-add=NET_RAW".to_string(),
        "--cap-add=SYS_ADMIN".to_string(),
        "--device".to_string(), "/dev/net/tun".to_string(),
        "--security-opt".to_string(), "label=disable".to_string(),
    ];

    if let Some(c) = cpus {
        if c > 0.0 {
            podman_args.push(format!("--cpus={}", c));
        }
    }
    if let Some(m) = memory_mb {
        if m > 0 {
            podman_args.push(format!("--memory={}m", m));
        }
    }

    podman_args.extend([
        "-e".to_string(), "NETWORK_MODE=offline".to_string(),
        "-v".to_string(), py_mount,
        "-v".to_string(), out_mount,
        "run-python-sandbox".to_string(),
    ]);

    let output = Command::new("podman")
        .args(&podman_args)
        .output()
        .await
        .context("Failed to execute podman container")?;

    let elapsed_ms = start_time.elapsed().as_millis() as i64;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let exit_code = output.status.code().unwrap_or(-1);

    // Read and parse internal metrics if generated
    let mut metrics = Metrics {
        wall_time_ms: elapsed_ms,
        max_memory_kb: 0,
        cpu_percentage: "0%".to_string(),
        user_time_sec: 0.0,
        sys_time_sec: 0.0,
        fs_inputs: 0,
        fs_outputs: 0,
        voluntary_context_switches: 0,
        involuntary_context_switches: 0,
    };

    let metrics_path = out_dir.join("metrics.json");
    if metrics_path.is_file() {
        if let Ok(content) = fs::read_to_string(&metrics_path).await {
            #[derive(Deserialize)]
            struct InnerMetrics {
                max_memory_kb: i64,
                cpu_percentage: String,
                user_time_sec: f64,
                sys_time_sec: f64,
                fs_inputs: i64,
                fs_outputs: i64,
                voluntary_context_switches: i64,
                involuntary_context_switches: i64,
            }
            if let Ok(inner) = serde_json::from_str::<InnerMetrics>(&content) {
                metrics.max_memory_kb = inner.max_memory_kb;
                metrics.cpu_percentage = inner.cpu_percentage;
                metrics.user_time_sec = inner.user_time_sec;
                metrics.sys_time_sec = inner.sys_time_sec;
                metrics.fs_inputs = inner.fs_inputs;
                metrics.fs_outputs = inner.fs_outputs;
                metrics.voluntary_context_switches = inner.voluntary_context_switches;
                metrics.involuntary_context_switches = inner.involuntary_context_switches;
            }
        }
        // Delete the metrics file from host
        let _ = fs::remove_file(&metrics_path).await;
    }

    // 3. Read output files and convert to base64
    let mut output_files = HashMap::new();
    let mut dir_entries = fs::read_dir(&out_dir)
        .await
        .context("Failed to read output directory contents")?;

    while let Some(entry) = dir_entries.next_entry().await? {
        let path = entry.path();
        if path.is_file() && path.file_name() != Some(std::ffi::OsStr::new("metrics.json")) {
            if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                let content = fs::read(&path)
                    .await
                    .with_context(|| format!("Failed to read output file: {}", file_name))?;
                let encoded = BASE64_STANDARD.encode(content);
                output_files.insert(file_name.to_string(), encoded);
            }
        }
    }

    // Clean up temporary files asynchronously in background
    let _cleanup_run = tokio::spawn(async move {
        let _ = fs::remove_dir_all(&run_dir).await;
        let _ = fs::remove_dir_all(&out_dir).await;
    });

    Ok(RunResponse {
        stdout,
        stderr,
        exit_code,
        metrics,
        output_files,
    })
}

/// Changes the target directory permissions to 777.
///
/// This permits the container’s subuid mapped users to write files into it.
///
/// # Errors
/// Returns an error if setting directory permissions fails.
async fn set_directory_writable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, std::fs::Permissions::from_mode(0o777))
            .await
            .context("Failed to set unix folder permissions")?;
    }
    Ok(())
}
