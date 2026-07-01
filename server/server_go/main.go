package main

import (
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"syscall"
	"time"
)

// RunRequest represents the payload accepted by the POST /run endpoint.
type RunRequest struct {
	Code     string  `json:"code"`
	Network  string  `json:"network"` // offline, isolated, full
	CPUs     float64 `json:"cpus"`
	MemoryMB int64   `json:"memory_mb"`
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
}

func handleRun(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "Only POST requests are allowed", http.StatusMethodNotAllowed)
		return
	}

	body, err := io.ReadAll(r.Body)
	if err != nil {
		http.Error(w, "Failed to read request body", http.StatusBadRequest)
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

	// 3. Prepare the podman run command
	cmdArgs := []string{
		"run", "--rm",
		"--cap-add=NET_ADMIN",
		"--cap-add=NET_RAW",
		"--cap-add=SYS_ADMIN",
		"--device", "/dev/net/tun",
		"--security-opt", "label=disable",
	}

	if req.CPUs > 0 {
		cmdArgs = append(cmdArgs, fmt.Sprintf("--cpus=%g", req.CPUs))
	}
	if req.MemoryMB > 0 {
		cmdArgs = append(cmdArgs, fmt.Sprintf("--memory=%dm", req.MemoryMB))
	}

	cmdArgs = append(cmdArgs,
		"-e", "NETWORK_MODE=offline",
		"-v", pyFilePath+":/sandbox/run.py:ro",
		"-v", outDir+":/output:rw",
		"run-python-sandbox",
	)

	cmd := exec.Command("podman", cmdArgs...)

	// Create pipes for stdout/stderr to capture execution run
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

	log.Printf("[Go Worker] Spawning container with network=%s...", req.Network)
	startTime := time.Now()
	if err := cmd.Start(); err != nil {
		http.Error(w, fmt.Sprintf("Failed to start podman container: %v", err), http.StatusInternalServerError)
		return
	}

	// Read outputs concurrently
	stdoutBytes, _ := io.ReadAll(stdoutPipe)
	stderrBytes, _ := io.ReadAll(stderrPipe)

	exitCode := 0
	if err := cmd.Wait(); err != nil {
		if exitError, ok := err.(*exec.ExitError); ok {
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

	// Parse internal metrics if generated
	metrics := Metrics{
		WallTimeMs:                 elapsedMs,
		MaxMemoryKb:                0,
		CpuPercentage:              "0%",
		UserTimeSec:                0.0,
		SysTimeSec:                 0.0,
		FsInputs:                   0,
		FsOutputs:                  0,
		VoluntaryContextSwitches:   0,
		InvoluntaryContextSwitches: 0,
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
		// Delete the metrics file from host so it's clean
		_ = os.Remove(metricsFilePath)
	}

	outputFiles := make(map[string]string)
	for _, file := range files {
		if file.Type().IsRegular() && file.Name() != "metrics.json" {
			filePath := filepath.Join(outDir, file.Name())
			content, err := os.ReadFile(filePath)
			if err != nil {
				log.Printf("[Go Worker] Warning: Failed to read output file %s: %v", file.Name(), err)
				continue
			}
			// Encode in base64 to handle binary files safely
			outputFiles[file.Name()] = base64.StdEncoding.EncodeToString(content)
		}
	}

	// 5. Build response JSON
	resp := RunResponse{
		Stdout:      string(stdoutBytes),
		Stderr:      string(stderrBytes),
		ExitCode:    exitCode,
		Metrics:     metrics,
		OutputFiles: outputFiles,
		RunID:       guid,
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

func main() {
	port := "8080"
	if envPort := os.Getenv("PORT"); envPort != "" {
		port = envPort
	}

	http.HandleFunc("/", enableCORS(handleIndex))
	http.HandleFunc("/run", enableCORS(handleRun))
	http.HandleFunc("/libraries", enableCORS(handleLibraries))
	http.HandleFunc("/tiff.min.js", enableCORS(handleTiff))
	log.Printf("[Go Worker Service] Listening on port %s...", port)
	if err := http.ListenAndServe(":"+port, nil); err != nil {
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
