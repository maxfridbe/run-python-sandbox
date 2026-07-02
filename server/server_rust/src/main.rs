// Rust guideline compliant 2026-02-21

use anyhow::{anyhow, Context, Result};
use axum::{
    extract::{DefaultBodyLimit, Request},
    http::{HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::prelude::*;
use mimalloc::MiMalloc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Debug;
use std::path::Path;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio::time::timeout;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// Server-enforced safety limits sourced from environment variables. Every
/// request is clamped to these ceilings regardless of client-supplied values.
#[derive(Debug)]
struct Config {
    bind: String,
    port: String,
    token: String,
    timeout_sec: u64,
    max_concurrency: usize,
    max_cpus: f64,
    default_cpus: f64,
    max_memory_mb: i64,
    default_mem_mb: i64,
    pids_limit: i64,
    max_body_bytes: usize,
    max_stream_bytes: u64,
    max_output_bytes: u64,
    seccomp_profile: String,
}

static CFG: OnceLock<Config> = OnceLock::new();
static SEM: OnceLock<Semaphore> = OnceLock::new();

fn cfg() -> &'static Config {
    CFG.get().expect("config initialized in main")
}

fn env_str(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_int<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn env_float(key: &str, default: f64) -> f64 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn load_config() -> Config {
    Config {
        bind: env_str("SANDBOX_BIND", "127.0.0.1"),
        port: env_str("PORT", "8081"),
        token: env_str("SANDBOX_TOKEN", ""),
        timeout_sec: env_int("SANDBOX_TIMEOUT_SEC", 60),
        max_concurrency: env_int("SANDBOX_MAX_CONCURRENCY", 4),
        max_cpus: env_float("SANDBOX_MAX_CPUS", 2.0),
        default_cpus: env_float("SANDBOX_DEFAULT_CPUS", 1.0),
        max_memory_mb: env_int("SANDBOX_MAX_MEMORY_MB", 2048),
        default_mem_mb: env_int("SANDBOX_DEFAULT_MEMORY_MB", 1024),
        pids_limit: env_int("SANDBOX_PIDS_LIMIT", 256),
        max_body_bytes: env_int::<usize>("SANDBOX_MAX_BODY_MB", 10) * 1024 * 1024,
        max_stream_bytes: env_int::<u64>("SANDBOX_MAX_LOG_MB", 4) * 1024 * 1024,
        max_output_bytes: env_int::<u64>("SANDBOX_MAX_OUTPUT_MB", 32) * 1024 * 1024,
        seccomp_profile: resolve_seccomp_profile(),
    }
}

/// Resolves the seccomp profile path to pass to podman, or "" for the default.
/// `SANDBOX_SECCOMP` (explicit path) wins; otherwise a truthy `SANDBOX_HARDENED`
/// selects the bundled hardened profile.
fn resolve_seccomp_profile() -> String {
    if let Ok(p) = std::env::var("SANDBOX_SECCOMP") {
        if !p.is_empty() {
            return std::fs::canonicalize(&p).map(|c| c.display().to_string()).unwrap_or(p);
        }
    }
    let hardened = std::env::var("SANDBOX_HARDENED").unwrap_or_default();
    if hardened == "1" || hardened == "true" || hardened == "yes" {
        for c in ["host/seccomp-hardened.json", "../host/seccomp-hardened.json", "../../host/seccomp-hardened.json", "seccomp-hardened.json"] {
            if let Ok(abs) = std::fs::canonicalize(c) {
                return abs.display().to_string();
            }
        }
        eprintln!("[Rust Worker] WARNING: SANDBOX_HARDENED set but seccomp-hardened.json not found; using default seccomp.");
    }
    String::new()
}

/// Forces a CPU request into `(0, max_cpus]`, substituting the default for any
/// non-positive value (which a client could use to request "unlimited").
fn clamp_cpus(req: Option<f64>) -> f64 {
    let c = cfg();
    let mut v = req.unwrap_or(0.0);
    if v <= 0.0 {
        v = c.default_cpus;
    }
    if v > c.max_cpus {
        v = c.max_cpus;
    }
    v
}

fn clamp_memory(req: Option<i64>) -> i64 {
    let c = cfg();
    let mut v = req.unwrap_or(0);
    if v <= 0 {
        v = c.default_mem_mb;
    }
    if v > c.max_memory_mb {
        v = c.max_memory_mb;
    }
    v
}

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
    /// Map of input files filename -> base64_content.
    pub input_files: Option<HashMap<String, String>>,
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
    /// Unique execution trace ID.
    pub run_id: String,
    /// True when the container was killed for exceeding the execution timeout.
    pub timed_out: bool,
}

/// CORS middleware applying permissive headers and short-circuiting preflight.
async fn cors_middleware(request: Request, next: Next) -> Response {
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

/// Enforces a bearer token when `SANDBOX_TOKEN` is set. When empty, auth is
/// skipped (intended for a loopback-only dev bind).
async fn auth_middleware(request: Request, next: Next) -> Response {
    let c = cfg();
    if !c.token.is_empty() && request.method() != Method::OPTIONS {
        let expected = format!("Bearer {}", c.token);
        let ok = request
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(|v| v == expected)
            .unwrap_or(false);
        if !ok {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    }
    next.run(request).await
}

#[tokio::main]
pub async fn main() -> Result<()> {
    let config = load_config();
    let bind = config.bind.clone();
    let port = config.port.clone();
    let max_body = config.max_body_bytes;
    let max_conc = config.max_concurrency;
    let has_token = !config.token.is_empty();

    let _ = SEM.set(Semaphore::new(max_conc));
    CFG.set(config).map_err(|_| anyhow!("config already set"))?;

    if !has_token && bind != "127.0.0.1" && bind != "localhost" {
        eprintln!("[Rust Worker Service] WARNING: no SANDBOX_TOKEN set while binding to {bind}; /run is unauthenticated.");
    }

    let app = Router::new()
        .route("/", get(handle_index))
        .route("/run", post(handle_run))
        .route("/libraries", get(handle_libraries))
        .route("/tiff.min.js", get(handle_tiff))
        .layer(axum::middleware::from_fn(auth_middleware))
        .layer(DefaultBodyLimit::max(max_body))
        .layer(axum::middleware::from_fn(cors_middleware));

    let addr = format!("{bind}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind to {addr}"))?;

    println!("[Rust Worker Service] Listening on: {addr} (max_concurrency={max_conc})");
    axum::serve(listener, app)
        .await
        .context("Server failed during execution")?;

    Ok(())
}

/// HTTP handler that returns a list of pre-installed Python libraries in the sandbox.
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
async fn handle_run(Json(payload): Json<RunRequest>) -> Result<Json<RunResponse>, StatusCode> {
    let network = payload.network.clone().unwrap_or_else(|| "offline".to_string());
    if network != "offline" && network != "isolated" && network != "full" {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Bound concurrent executions; reject fast instead of queueing unboundedly.
    let _permit = match SEM.get().expect("sem initialized").try_acquire() {
        Ok(p) => p,
        Err(_) => return Err(StatusCode::TOO_MANY_REQUESTS),
    };

    match execute_sandbox(&payload.code, &network, payload.cpus, payload.memory_mb, payload.input_files.unwrap_or_default()).await {
        Ok(response) => Ok(Json(response)),
        Err(e) => {
            eprintln!("[Rust Worker] Execution error: {e:?}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Generates a unique execution run ID based on /proc/sys/kernel/random/uuid.
fn generate_run_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/uuid")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            format!("rust-fallback-{nanos}")
        })
}

/// Reads an async stream into memory, but never more than `limit` bytes, so an
/// infinite print loop in the sandbox cannot exhaust server memory.
async fn read_capped<R: AsyncRead + Unpin>(reader: R, limit: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = reader.take(limit).read_to_end(&mut buf).await;
    buf
}

/// Best-effort force removal of a container by name.
async fn force_remove(name: &str) {
    let _ = Command::new("podman").args(["rm", "-f", name]).output().await;
}

/// Performs filesystem preparation, asynchronous Podman invocation, and result ingestion.
async fn execute_sandbox(
    code: &str,
    network: &str,
    cpus: Option<f64>,
    memory_mb: Option<i64>,
    input_files: HashMap<String, String>,
) -> Result<RunResponse> {
    let c = cfg();
    let run_id = generate_run_id();
    println!("[Rust Worker] [Request {run_id}] Processing execution request");
    let temp_dir = std::path::Path::new("/tmp");

    let run_dir = temp_dir.join(format!("sandbox-run-rust-{run_id}"));
    let out_dir = temp_dir.join(format!("sandbox-out-{run_id}"));
    let in_dir = temp_dir.join(format!("sandbox-in-{run_id}"));

    fs::create_dir_all(&run_dir).await.context("Failed to create execution run directory")?;
    fs::create_dir_all(&out_dir).await.context("Failed to create output directory")?;
    fs::create_dir_all(&in_dir).await.context("Failed to create input directory")?;

    set_directory_writable(&out_dir).await?;

    let py_file_path = run_dir.join("run.py");
    fs::write(&py_file_path, code).await.context("Failed to write python script to run_dir")?;

    for (fname, b64_content) in input_files {
        if let Some(safe_name) = Path::new(&fname).file_name() {
            if let Ok(decoded) = BASE64_STANDARD.decode(b64_content.trim()) {
                let _ = fs::write(in_dir.join(safe_name), decoded).await;
            }
        }
    }

    let py_mount = format!("{}:/sandbox/run.py:ro", py_file_path.display());
    let out_mount = format!("{}:/output:rw", out_dir.display());
    let in_mount = format!("{}:/input:ro", in_dir.display());

    let cpus_val = clamp_cpus(cpus);
    let mem_val = clamp_memory(memory_mb);
    let container_name = format!("sandbox-rust-{run_id}");

    println!(
        "[Rust Worker] Spawning container network={network} (cpus={cpus_val} mem={mem_val}MB timeout={}s)...",
        c.timeout_sec
    );
    let start_time = std::time::Instant::now();

    let mut podman_args = vec![
        "run".to_string(), "--rm".to_string(),
        "--name".to_string(), container_name.clone(),
        "--cap-add=NET_ADMIN".to_string(),
        "--cap-add=NET_RAW".to_string(),
        "--cap-add=SYS_ADMIN".to_string(),
        "--device".to_string(), "/dev/net/tun".to_string(),
        "--security-opt".to_string(), "label=disable".to_string(),
    ];
    if !c.seccomp_profile.is_empty() {
        podman_args.push("--security-opt".to_string());
        podman_args.push(format!("seccomp={}", c.seccomp_profile));
    }
    podman_args.extend([
        format!("--cpus={cpus_val}"),
        format!("--memory={mem_val}m"),
        format!("--pids-limit={}", c.pids_limit),
        format!("--timeout={}", c.timeout_sec),
        "-e".to_string(), "NETWORK_MODE=offline".to_string(),
        "-v".to_string(), py_mount,
        "-v".to_string(), out_mount,
        "-v".to_string(), in_mount,
        "run-python-sandbox".to_string(),
    ]);

    let mut child = Command::new("podman")
        .args(&podman_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to spawn podman container")?;

    let stdout = child.stdout.take().context("missing stdout pipe")?;
    let stderr = child.stderr.take().context("missing stderr pipe")?;
    let out_task = tokio::spawn(read_capped(stdout, c.max_stream_bytes));
    let err_task = tokio::spawn(read_capped(stderr, c.max_stream_bytes));

    // Backstop deadline in case the podman CLI wedges; podman --timeout should
    // stop the container first.
    let deadline = Duration::from_secs(c.timeout_sec + 15);
    let mut timed_out = false;
    let exit_code: i32 = match timeout(deadline, child.wait()).await {
        Ok(Ok(status)) => status.code().unwrap_or(-1),
        Ok(Err(e)) => return Err(anyhow!("Failed to wait on podman: {e}")),
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            force_remove(&container_name).await;
            timed_out = true;
            124
        }
    };

    let elapsed_ms = start_time.elapsed().as_millis() as i64;
    let stdout = String::from_utf8_lossy(&out_task.await.unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&err_task.await.unwrap_or_default()).into_owned();

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
        let _ = fs::remove_file(&metrics_path).await;
    }

    // Read output files up to a total byte budget. We inspect the directory
    // entry's own file type (no symlink traversal) so sandboxed code cannot
    // plant a symlink pointing at arbitrary host files.
    let mut output_files = HashMap::new();
    let mut budget: u64 = c.max_output_bytes;
    let mut dir_entries = fs::read_dir(&out_dir).await.context("Failed to read output directory contents")?;
    while let Some(entry) = dir_entries.next_entry().await? {
        let file_type = match entry.file_type().await {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        let file_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) if n != "metrics.json" => n.to_string(),
            _ => continue,
        };
        let size = entry.metadata().await.map(|m| m.len()).unwrap_or(u64::MAX);
        if size > budget {
            eprintln!("[Rust Worker] Skipping output file {file_name}: exceeds remaining output budget");
            continue;
        }
        if let Ok(content) = fs::read(&path).await {
            budget = budget.saturating_sub(content.len() as u64);
            output_files.insert(file_name, BASE64_STANDARD.encode(content));
        }
    }

    let _cleanup = tokio::spawn(async move {
        let _ = fs::remove_dir_all(&run_dir).await;
        let _ = fs::remove_dir_all(&out_dir).await;
        let _ = fs::remove_dir_all(&in_dir).await;
    });

    Ok(RunResponse {
        stdout,
        stderr,
        exit_code,
        metrics,
        output_files,
        run_id,
        timed_out,
    })
}

/// Changes the target directory permissions to 777 so the container's mapped
/// subuid can write output files into it.
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
