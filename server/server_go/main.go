package main

import (
	"context"
	"crypto/rand"
	"crypto/subtle"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"syscall"
	"time"
)

// Config holds the server-enforced safety limits. All values are sourced from
// environment variables so an operator can tune them, but every request is
// clamped to these ceilings regardless of what the client asks for.
type Config struct {
	Bind           string
	Port           string
	Token          string
	TimeoutSec     int
	MaxConcurrency int
	MaxCPUs        float64
	DefaultCPUs    float64
	MaxMemoryMB    int64
	DefaultMemMB   int64
	PidsLimit      int
	MaxBodyBytes   int64
	MaxStreamBytes int64
	MaxOutputBytes int64
	SeccompProfile string
}

var cfg Config

// sem bounds the number of concurrently executing sandbox containers so a burst
// of requests cannot fork-bomb the host with podman processes.
var sem chan struct{}

func loadConfig() Config {
	return Config{
		Bind:           getenvStr("SANDBOX_BIND", "127.0.0.1"),
		Port:           getenvStr("PORT", "8080"),
		Token:          os.Getenv("SANDBOX_TOKEN"),
		TimeoutSec:     getenvInt("SANDBOX_TIMEOUT_SEC", 60),
		MaxConcurrency: getenvInt("SANDBOX_MAX_CONCURRENCY", 4),
		MaxCPUs:        getenvFloat("SANDBOX_MAX_CPUS", 2.0),
		DefaultCPUs:    getenvFloat("SANDBOX_DEFAULT_CPUS", 1.0),
		MaxMemoryMB:    int64(getenvInt("SANDBOX_MAX_MEMORY_MB", 2048)),
		DefaultMemMB:   int64(getenvInt("SANDBOX_DEFAULT_MEMORY_MB", 1024)),
		PidsLimit:      getenvInt("SANDBOX_PIDS_LIMIT", 256),
		MaxBodyBytes:   int64(getenvInt("SANDBOX_MAX_BODY_MB", 10)) * 1024 * 1024,
		MaxStreamBytes: int64(getenvInt("SANDBOX_MAX_LOG_MB", 4)) * 1024 * 1024,
		MaxOutputBytes: int64(getenvInt("SANDBOX_MAX_OUTPUT_MB", 32)) * 1024 * 1024,
		SeccompProfile: resolveSeccompProfile(),
	}
}

// resolveSeccompProfile returns the absolute path of the seccomp profile to pass
// to podman, or "" for the default. SANDBOX_SECCOMP (explicit path) wins;
// otherwise a truthy SANDBOX_HARDENED selects the bundled hardened profile.
func resolveSeccompProfile() string {
	if p := os.Getenv("SANDBOX_SECCOMP"); p != "" {
		if abs, err := filepath.Abs(p); err == nil {
			return abs
		}
		return p
	}
	if v := os.Getenv("SANDBOX_HARDENED"); v == "1" || v == "true" || v == "yes" {
		candidates := []string{
			"host/seccomp-hardened.json",
			"../host/seccomp-hardened.json",
			"../../host/seccomp-hardened.json",
			"seccomp-hardened.json",
		}
		for _, c := range candidates {
			if abs, err := filepath.Abs(c); err == nil {
				if _, err := os.Stat(abs); err == nil {
					return abs
				}
			}
		}
		log.Printf("[Go Worker] WARNING: SANDBOX_HARDENED set but seccomp-hardened.json not found; using default seccomp.")
	}
	return ""
}

func getenvStr(key, def string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return def
}

func getenvInt(key string, def int) int {
	if v := os.Getenv(key); v != "" {
		if n, err := strconv.Atoi(v); err == nil {
			return n
		}
	}
	return def
}

func getenvFloat(key string, def float64) float64 {
	if v := os.Getenv(key); v != "" {
		if n, err := strconv.ParseFloat(v, 64); err == nil {
			return n
		}
	}
	return def
}

// clampCPUs forces a request into (0, MaxCPUs]. A request of <= 0 (which the
// client could use to ask for "unlimited") is replaced by the default.
func clampCPUs(req float64) float64 {
	if req <= 0 {
		req = cfg.DefaultCPUs
	}
	if req > cfg.MaxCPUs {
		req = cfg.MaxCPUs
	}
	return req
}

func clampMemory(req int64) int64 {
	if req <= 0 {
		req = cfg.DefaultMemMB
	}
	if req > cfg.MaxMemoryMB {
		req = cfg.MaxMemoryMB
	}
	return req
}

// RunRequest represents the payload accepted by the POST /run endpoint.
type RunRequest struct {
	Code       string            `json:"code"`
	Network    string            `json:"network"` // offline, isolated, full
	CPUs       float64           `json:"cpus"`
	MemoryMB   int64             `json:"memory_mb"`
	InputFiles map[string]string `json:"input_files"`
}

// Metrics represents the sandbox execution resource metrics.
type Metrics struct {
	WallTimeMs                 int64   `json:"wall_time_ms"`
	MaxMemoryKb                int64   `json:"max_memory_kb"`
	CpuPercentage              string  `json:"cpu_percentage"`
	UserTimeSec                float64 `json:"user_time_sec"`
	SysTimeSec                 float64 `json:"sys_time_sec"`
	FsInputs                   int64   `json:"fs_inputs"`
	FsOutputs                  int64   `json:"fs_outputs"`
	VoluntaryContextSwitches   int64   `json:"voluntary_context_switches"`
	InvoluntaryContextSwitches int64   `json:"involuntary_context_switches"`
}

// RunResponse represents the execution results returned to the client.
type RunResponse struct {
	Stdout      string            `json:"stdout"`
	Stderr      string            `json:"stderr"`
	ExitCode    int               `json:"exit_code"`
	Metrics     Metrics           `json:"metrics"`
	OutputFiles map[string]string `json:"output_files"`
	RunID       string            `json:"run_id"`
	TimedOut    bool              `json:"timed_out"`
}

func handleRun(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "Only POST requests are allowed", http.StatusMethodNotAllowed)
		return
	}

	// Bound request body size to protect the server from memory-exhaustion via
	// a huge code blob or input_files payload.
	r.Body = http.MaxBytesReader(w, r.Body, cfg.MaxBodyBytes)
	body, err := io.ReadAll(r.Body)
	if err != nil {
		http.Error(w, "Request body too large or unreadable", http.StatusRequestEntityTooLarge)
		return
	}

	var req RunRequest
	if err := json.Unmarshal(body, &req); err != nil {
		http.Error(w, "Invalid JSON payload", http.StatusBadRequest)
		return
	}

	// Default network mode is offline
	if req.Network == "" {
		req.Network = "offline"
	}
	if req.Network != "offline" && req.Network != "isolated" && req.Network != "full" {
		http.Error(w, "Network must be 'offline', 'isolated', or 'full'", http.StatusBadRequest)
		return
	}

	// Acquire a concurrency slot or reject fast so we never queue unboundedly.
	select {
	case sem <- struct{}{}:
		defer func() { <-sem }()
	default:
		http.Error(w, "Server busy: too many concurrent executions", http.StatusTooManyRequests)
		return
	}

	guid := generateUUID()
	log.Printf("[Go Worker] [Request %s] Processing execution request", guid)

	// 1. Create a temp directory for the execution run
	runDir := filepath.Join("/tmp", fmt.Sprintf("sandbox-run-go-%s", guid))
	if err := os.MkdirAll(runDir, 0755); err != nil {
		http.Error(w, fmt.Sprintf("Failed to create run directory: %v", err), http.StatusInternalServerError)
		return
	}
	defer os.RemoveAll(runDir)

	// Write the Python script to run.py
	pyFilePath := filepath.Join(runDir, "run.py")
	if err := os.WriteFile(pyFilePath, []byte(req.Code), 0644); err != nil {
		http.Error(w, fmt.Sprintf("Failed to write python script: %v", err), http.StatusInternalServerError)
		return
	}

	// 2. Create a temp directory for outputs
	outDir := filepath.Join("/tmp", fmt.Sprintf("sandbox-out-%s", guid))
	if err := os.MkdirAll(outDir, 0777); err != nil {
		http.Error(w, fmt.Sprintf("Failed to create output directory: %v", err), http.StatusInternalServerError)
		return
	}
	defer os.RemoveAll(outDir)

	// We MUST chmod the host output directory to 777 so the unprivileged container UID
	// can write output files into it.
	if err := os.Chmod(outDir, 0777); err != nil {
		http.Error(w, fmt.Sprintf("Failed to chmod output directory: %v", err), http.StatusInternalServerError)
		return
	}

	// 2b. Create a temp directory for input files
	inDir := filepath.Join("/tmp", fmt.Sprintf("sandbox-in-%s", guid))
	if err := os.MkdirAll(inDir, 0755); err != nil {
		http.Error(w, fmt.Sprintf("Failed to create input directory: %v", err), http.StatusInternalServerError)
		return
	}
	defer os.RemoveAll(inDir)

	for fname, b64Content := range req.InputFiles {
		fname = filepath.Base(fname)
		b, err := base64.StdEncoding.DecodeString(b64Content)
		if err != nil {
			http.Error(w, fmt.Sprintf("Failed to decode input file %s: %v", fname, err), http.StatusBadRequest)
			return
		}
		if err := os.WriteFile(filepath.Join(inDir, fname), b, 0644); err != nil {
			http.Error(w, fmt.Sprintf("Failed to write input file %s: %v", fname, err), http.StatusInternalServerError)
			return
		}
	}

	// 3. Prepare the podman run command with server-enforced resource guards.
	cpus := clampCPUs(req.CPUs)
	memMB := clampMemory(req.MemoryMB)
	containerName := "sandbox-go-" + guid

	cmdArgs := []string{
		"run", "--rm",
		"--name", containerName,
		"--cap-add=NET_ADMIN",
		"--cap-add=NET_RAW",
		"--cap-add=SYS_ADMIN",
		"--device", "/dev/net/tun",
		"--security-opt", "label=disable",
	}
	if cfg.SeccompProfile != "" {
		cmdArgs = append(cmdArgs, "--security-opt", "seccomp="+cfg.SeccompProfile)
	}
	cmdArgs = append(cmdArgs,
		fmt.Sprintf("--cpus=%g", cpus),
		fmt.Sprintf("--memory=%dm", memMB),
		fmt.Sprintf("--pids-limit=%d", cfg.PidsLimit),
		fmt.Sprintf("--timeout=%d", cfg.TimeoutSec),
	)

	cmdArgs = append(cmdArgs,
		"-e", "NETWORK_MODE=offline",
		"-v", pyFilePath+":/sandbox/run.py:ro",
		"-v", outDir+":/output:rw",
		"-v", inDir+":/input:ro",
		"run-python-sandbox",
	)

	// Hard client-side deadline as a backstop in case the podman CLI itself
	// wedges; podman's own --timeout should stop the container first.
	ctx, cancel := context.WithTimeout(context.Background(), time.Duration(cfg.TimeoutSec+15)*time.Second)
	defer cancel()
	// Ensure the container is force-removed even if podman leaks it on timeout.
	defer func() {
		rm := exec.Command("podman", "rm", "-f", containerName)
		_ = rm.Run()
	}()

	cmd := exec.CommandContext(ctx, "podman", cmdArgs...)

	stdoutPipe, err := cmd.StdoutPipe()
	if err != nil {
		http.Error(w, fmt.Sprintf("Failed to create stdout pipe: %v", err), http.StatusInternalServerError)
		return
	}
	stderrPipe, err := cmd.StderrPipe()
	if err != nil {
		http.Error(w, fmt.Sprintf("Failed to create stderr pipe: %v", err), http.StatusInternalServerError)
		return
	}

	log.Printf("[Go Worker] Spawning container (cpus=%g mem=%dMB timeout=%ds)...", cpus, memMB, cfg.TimeoutSec)
	startTime := time.Now()
	if err := cmd.Start(); err != nil {
		http.Error(w, fmt.Sprintf("Failed to start podman container: %v", err), http.StatusInternalServerError)
		return
	}

	// Read outputs with a hard cap so an infinite print loop cannot exhaust
	// server memory.
	stdoutBytes, _ := io.ReadAll(io.LimitReader(stdoutPipe, cfg.MaxStreamBytes))
	stderrBytes, _ := io.ReadAll(io.LimitReader(stderrPipe, cfg.MaxStreamBytes))

	exitCode := 0
	timedOut := false
	if err := cmd.Wait(); err != nil {
		if ctx.Err() == context.DeadlineExceeded {
			timedOut = true
			exitCode = 124
		} else if exitError, ok := err.(*exec.ExitError); ok {
			ws := exitError.Sys().(syscall.WaitStatus)
			exitCode = ws.ExitStatus()
		} else {
			http.Error(w, fmt.Sprintf("Error during execution wait: %v", err), http.StatusInternalServerError)
			return
		}
	}
	elapsedMs := time.Since(startTime).Milliseconds()

	// 4. Ingest output files from host outDir
	files, err := os.ReadDir(outDir)
	if err != nil {
		http.Error(w, fmt.Sprintf("Failed to read output directory contents: %v", err), http.StatusInternalServerError)
		return
	}

	metrics := Metrics{
		WallTimeMs:    elapsedMs,
		CpuPercentage: "0%",
	}

	metricsFilePath := filepath.Join(outDir, "metrics.json")
	if metricsBytes, err := os.ReadFile(metricsFilePath); err == nil {
		var innerMetrics struct {
			MaxMemoryKb                int64   `json:"max_memory_kb"`
			CpuPercentage              string  `json:"cpu_percentage"`
			UserTimeSec                float64 `json:"user_time_sec"`
			SysTimeSec                 float64 `json:"sys_time_sec"`
			FsInputs                   int64   `json:"fs_inputs"`
			FsOutputs                  int64   `json:"fs_outputs"`
			VoluntaryContextSwitches   int64   `json:"voluntary_context_switches"`
			InvoluntaryContextSwitches int64   `json:"involuntary_context_switches"`
		}
		if err := json.Unmarshal(metricsBytes, &innerMetrics); err == nil {
			metrics.MaxMemoryKb = innerMetrics.MaxMemoryKb
			metrics.CpuPercentage = innerMetrics.CpuPercentage
			metrics.UserTimeSec = innerMetrics.UserTimeSec
			metrics.SysTimeSec = innerMetrics.SysTimeSec
			metrics.FsInputs = innerMetrics.FsInputs
			metrics.FsOutputs = innerMetrics.FsOutputs
			metrics.VoluntaryContextSwitches = innerMetrics.VoluntaryContextSwitches
			metrics.InvoluntaryContextSwitches = innerMetrics.InvoluntaryContextSwitches
		}
		_ = os.Remove(metricsFilePath)
	}

	// Read regular output files up to a total byte budget. IsRegular() skips
	// symlinks, so sandboxed code cannot point us at arbitrary host files.
	outputFiles := make(map[string]string)
	var outputBudget = cfg.MaxOutputBytes
	for _, file := range files {
		if !file.Type().IsRegular() || file.Name() == "metrics.json" {
			continue
		}
		info, err := file.Info()
		if err != nil {
			continue
		}
		if info.Size() > outputBudget {
			log.Printf("[Go Worker] Skipping output file %s: exceeds remaining output budget", file.Name())
			continue
		}
		filePath := filepath.Join(outDir, file.Name())
		content, err := os.ReadFile(filePath)
		if err != nil {
			log.Printf("[Go Worker] Warning: Failed to read output file %s: %v", file.Name(), err)
			continue
		}
		outputBudget -= int64(len(content))
		outputFiles[file.Name()] = base64.StdEncoding.EncodeToString(content)
	}

	resp := RunResponse{
		Stdout:      string(stdoutBytes),
		Stderr:      string(stderrBytes),
		ExitCode:    exitCode,
		Metrics:     metrics,
		OutputFiles: outputFiles,
		RunID:       guid,
		TimedOut:    timedOut,
	}

	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(resp); err != nil {
		log.Printf("[Go Worker] Error encoding response: %v", err)
	}
}

func handleIndex(w http.ResponseWriter, r *http.Request) {
	if r.URL.Path != "/" {
		http.NotFound(w, r)
		return
	}
	paths := []string{"wfe/index.html", "../wfe/index.html", "../../wfe/index.html", "index.html"}
	var content []byte
	var err error
	for _, p := range paths {
		content, err = os.ReadFile(p)
		if err == nil {
			break
		}
	}
	if err != nil {
		http.Error(w, "index.html not found", http.StatusNotFound)
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	w.Write(content)
}

func handleLibraries(w http.ResponseWriter, r *http.Request) {
	// Query available libraries from container
	cmd := exec.Command("podman", "run", "--rm", "run-python-sandbox", "python3", "-c",
		"import importlib.metadata, json; print(json.dumps([d.metadata['Name'] for d in importlib.metadata.distributions()]))")
	out, err := cmd.Output()

	var libs []string
	if err == nil {
		_ = json.Unmarshal(out, &libs)
	}

	// Fallback + built-ins
	builtins := []string{"os", "sys", "json", "math", "urllib", "time", "subprocess", "random", "re"}

	// Filter and convert to lower/standard names
	seen := make(map[string]bool)
	var finalLibs []string

	for _, b := range builtins {
		seen[b] = true
		finalLibs = append(finalLibs, b)
	}

	// Map known package names to their python import names
	packageMap := map[string]string{
		"Pillow":    "PIL",
		"reportlab": "reportlab",
		"pypdf":     "pypdf",
	}

	for _, l := range libs {
		importName := l
		if val, ok := packageMap[l]; ok {
			importName = val
		}
		if !seen[importName] {
			seen[importName] = true
			finalLibs = append(finalLibs, importName)
		}
	}

	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(finalLibs)
}

func handleTiff(w http.ResponseWriter, r *http.Request) {
	paths := []string{"wfe/tiff.min.js", "../wfe/tiff.min.js", "../../wfe/tiff.min.js", "tiff.min.js"}
	var content []byte
	var err error
	for _, p := range paths {
		content, err = os.ReadFile(p)
		if err == nil {
			break
		}
	}
	if err != nil {
		http.Error(w, "tiff.min.js not found", http.StatusNotFound)
		return
	}
	w.Header().Set("Content-Type", "application/javascript")
	w.Write(content)
}

func enableCORS(next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Access-Control-Allow-Origin", "*")
		w.Header().Set("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
		w.Header().Set("Access-Control-Allow-Headers", "Content-Type, Authorization")
		if r.Method == "OPTIONS" {
			w.WriteHeader(http.StatusOK)
			return
		}
		next(w, r)
	}
}

// requireAuth enforces a bearer token when SANDBOX_TOKEN is configured. When it
// is empty, auth is skipped (intended for a loopback-only dev bind).
func requireAuth(next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if cfg.Token != "" && r.Method != http.MethodOptions {
			expected := "Bearer " + cfg.Token
			got := r.Header.Get("Authorization")
			if subtle.ConstantTimeCompare([]byte(got), []byte(expected)) != 1 {
				http.Error(w, "Unauthorized", http.StatusUnauthorized)
				return
			}
		}
		next(w, r)
	}
}

func main() {
	cfg = loadConfig()
	sem = make(chan struct{}, cfg.MaxConcurrency)

	if cfg.Token == "" && cfg.Bind != "127.0.0.1" && cfg.Bind != "localhost" {
		log.Printf("[Go Worker Service] WARNING: no SANDBOX_TOKEN set while binding to %s; the /run endpoint is unauthenticated.", cfg.Bind)
	}

	http.HandleFunc("/", enableCORS(handleIndex))
	http.HandleFunc("/run", enableCORS(requireAuth(handleRun)))
	http.HandleFunc("/libraries", enableCORS(handleLibraries))
	http.HandleFunc("/tiff.min.js", enableCORS(handleTiff))

	addr := cfg.Bind + ":" + cfg.Port
	log.Printf("[Go Worker Service] Listening on %s (max_concurrency=%d, timeout=%ds, max_cpus=%g, max_mem=%dMB)...",
		addr, cfg.MaxConcurrency, cfg.TimeoutSec, cfg.MaxCPUs, cfg.MaxMemoryMB)
	if err := http.ListenAndServe(addr, nil); err != nil {
		log.Fatalf("Server failed to start: %v", err)
	}
}

func generateUUID() string {
	b := make([]byte, 16)
	_, err := rand.Read(b)
	if err != nil {
		return fmt.Sprintf("%d", time.Now().UnixNano())
	}
	return fmt.Sprintf("%x-%x-%x-%x-%x", b[0:4], b[4:6], b[6:8], b[8:10], b[10:])
}
