using System.Diagnostics;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;

// Server-enforced safety limits sourced from environment variables. Every
// request is clamped to these ceilings regardless of client-supplied values.
static string EnvStr(string key, string def) => Environment.GetEnvironmentVariable(key) is { Length: > 0 } v ? v : def;
static int EnvInt(string key, int def) => int.TryParse(Environment.GetEnvironmentVariable(key), out var v) ? v : def;
static double EnvFloat(string key, double def) => double.TryParse(Environment.GetEnvironmentVariable(key), out var v) ? v : def;

var bindAddr = EnvStr("SANDBOX_BIND", "127.0.0.1");
var port = EnvStr("PORT", "8082");
var token = EnvStr("SANDBOX_TOKEN", "");
var timeoutSec = EnvInt("SANDBOX_TIMEOUT_SEC", 60);
var maxConcurrency = EnvInt("SANDBOX_MAX_CONCURRENCY", 4);
var maxCpus = EnvFloat("SANDBOX_MAX_CPUS", 2.0);
var defaultCpus = EnvFloat("SANDBOX_DEFAULT_CPUS", 1.0);
var maxMemoryMb = (long)EnvInt("SANDBOX_MAX_MEMORY_MB", 2048);
var defaultMemMb = (long)EnvInt("SANDBOX_DEFAULT_MEMORY_MB", 1024);
var pidsLimit = EnvInt("SANDBOX_PIDS_LIMIT", 256);
var maxBodyBytes = (long)EnvInt("SANDBOX_MAX_BODY_MB", 10) * 1024 * 1024;
var maxStreamChars = EnvInt("SANDBOX_MAX_LOG_MB", 4) * 1024 * 1024;
var maxOutputBytes = (long)EnvInt("SANDBOX_MAX_OUTPUT_MB", 32) * 1024 * 1024;

// Resolve the seccomp profile to pass to podman, or "" for the default.
// SANDBOX_SECCOMP (explicit path) wins; otherwise a truthy SANDBOX_HARDENED
// selects the bundled hardened profile.
static string ResolveSeccompProfile()
{
    var explicitPath = Environment.GetEnvironmentVariable("SANDBOX_SECCOMP");
    if (!string.IsNullOrEmpty(explicitPath))
    {
        return Path.GetFullPath(explicitPath);
    }
    var hardened = Environment.GetEnvironmentVariable("SANDBOX_HARDENED");
    if (hardened is "1" or "true" or "yes")
    {
        string[] candidates = { "host/seccomp-hardened.json", "../host/seccomp-hardened.json", "../../host/seccomp-hardened.json", "seccomp-hardened.json" };
        foreach (var candidate in candidates)
        {
            if (File.Exists(candidate)) return Path.GetFullPath(candidate);
        }
        Console.WriteLine("[Dotnet Worker] WARNING: SANDBOX_HARDENED set but seccomp-hardened.json not found; using default seccomp.");
    }
    return "";
}
var seccompProfile = ResolveSeccompProfile();

var runSemaphore = new SemaphoreSlim(maxConcurrency, maxConcurrency);

var builder = WebApplication.CreateBuilder(args);

// Bound request body size to protect the server from memory exhaustion.
builder.WebHost.ConfigureKestrel(options =>
{
    options.Limits.MaxRequestBodySize = maxBodyBytes;
});

// Enable CORS services
builder.Services.AddCors(options =>
{
    options.AddDefaultPolicy(policy =>
    {
        policy.AllowAnyOrigin()
              .AllowAnyMethod()
              .AllowAnyHeader();
    });
});

// Ensure we support JSON serialization options matching snake_case naming style
builder.Services.ConfigureHttpJsonOptions(options =>
{
    options.SerializerOptions.PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower;
    options.SerializerOptions.DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull;
});

var app = builder.Build();

// Enable CORS middleware
app.UseCors();

// Bearer-token auth for /run when SANDBOX_TOKEN is configured.
app.Use(async (context, next) =>
{
    if (!string.IsNullOrEmpty(token)
        && context.Request.Path == "/run"
        && !HttpMethods.IsOptions(context.Request.Method))
    {
        var auth = context.Request.Headers.Authorization.ToString();
        if (auth != $"Bearer {token}")
        {
            context.Response.StatusCode = StatusCodes.Status401Unauthorized;
            await context.Response.WriteAsync("Unauthorized");
            return;
        }
    }
    await next();
});

app.Urls.Clear();
app.Urls.Add($"http://{bindAddr}:{port}");

if (string.IsNullOrEmpty(token) && bindAddr != "127.0.0.1" && bindAddr != "localhost")
{
    Console.WriteLine($"[Dotnet Worker] WARNING: no SANDBOX_TOKEN set while binding to {bindAddr}; /run is unauthenticated.");
}

// Clamp CPU request into (0, maxCpus], substituting the default for non-positive.
double ClampCpus(double? req)
{
    var v = req ?? 0.0;
    if (v <= 0) v = defaultCpus;
    if (v > maxCpus) v = maxCpus;
    return v;
}

long ClampMemory(long? req)
{
    var v = req ?? 0;
    if (v <= 0) v = defaultMemMb;
    if (v > maxMemoryMb) v = maxMemoryMb;
    return v;
}

// Read a stream up to a character cap, then drain the rest so the child never
// blocks on a full pipe. Bounds server memory against infinite print loops.
async Task<string> ReadCappedAsync(StreamReader reader, int maxChars)
{
    var sb = new StringBuilder();
    var buffer = new char[8192];
    int read;
    while ((read = await reader.ReadAsync(buffer, 0, buffer.Length)) > 0)
    {
        var take = Math.Min(read, maxChars - sb.Length);
        if (take > 0) sb.Append(buffer, 0, take);
        if (sb.Length >= maxChars)
        {
            while (await reader.ReadAsync(buffer, 0, buffer.Length) > 0) { }
            break;
        }
    }
    return sb.ToString();
}

// 1. GET / - Serve index.html
app.MapGet("/", async (HttpContext context) =>
{
    string[] searchPaths = { "wfe/index.html", "../wfe/index.html", "../../wfe/index.html", "index.html" };
    foreach (var path in searchPaths)
    {
        if (File.Exists(path))
        {
            context.Response.ContentType = "text/html; charset=utf-8";
            await context.Response.SendFileAsync(path);
            return;
        }
    }
    context.Response.StatusCode = 404;
    await context.Response.WriteAsync("index.html not found");
});

// 2. GET /tiff.min.js - Serve tiff.min.js
app.MapGet("/tiff.min.js", async (HttpContext context) =>
{
    string[] searchPaths = { "wfe/tiff.min.js", "../wfe/tiff.min.js", "../../wfe/tiff.min.js", "tiff.min.js" };
    foreach (var path in searchPaths)
    {
        if (File.Exists(path))
        {
            context.Response.ContentType = "application/javascript";
            await context.Response.SendFileAsync(path);
            return;
        }
    }
    context.Response.StatusCode = 404;
    await context.Response.WriteAsync("tiff.min.js not found");
});

// 3. GET /libraries - Fetch available modules
app.MapGet("/libraries", async () =>
{
    var libs = new List<string>();
    try
    {
        var psi = new ProcessStartInfo
        {
            FileName = "podman",
            Arguments = "run --rm run-python-sandbox python3 -c \"import importlib.metadata, json; print(json.dumps([d.metadata['Name'] for d in importlib.metadata.distributions()]))\"",
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
            CreateNoWindow = true
        };
        using var process = Process.Start(psi);
        if (process != null)
        {
            string output = await process.StandardOutput.ReadToEndAsync();
            await process.WaitForExitAsync();
            if (process.ExitCode == 0)
            {
                var parsed = JsonSerializer.Deserialize<List<string>>(output);
                if (parsed != null)
                {
                    libs = parsed;
                }
            }
        }
    }
    catch (Exception ex)
    {
        Console.WriteLine($"[Dotnet Worker] Error checking container libs: {ex.Message}");
    }

    var builtins = new List<string> { "os", "sys", "json", "math", "urllib", "time", "subprocess", "random", "re" };
    var seen = new HashSet<string>(builtins);
    var finalLibs = new List<string>(builtins);

    var packageMap = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase)
    {
        { "Pillow", "PIL" },
        { "reportlab", "reportlab" },
        { "pypdf", "pypdf" }
    };

    foreach (var l in libs)
    {
        string importName = packageMap.TryGetValue(l, out var val) ? val : l;
        if (!seen.Contains(importName))
        {
            seen.Add(importName);
            finalLibs.Add(importName);
        }
    }

    return Results.Ok(finalLibs);
});

// 4. POST /run - Run script inside sandbox
app.MapPost("/run", async (RunRequest req) =>
{
    // Bound concurrent executions; reject fast instead of queueing unboundedly.
    if (!await runSemaphore.WaitAsync(0))
    {
        return Results.StatusCode(StatusCodes.Status429TooManyRequests);
    }

    try
    {
        var runId = Guid.NewGuid().ToString();
        Console.WriteLine($"[Dotnet Worker] [Request {runId}] Processing execution request");
        var runDir = Path.Combine("/tmp", $"sandbox-run-dotnet-{runId}");
        var outDir = Path.Combine("/tmp", $"sandbox-out-{runId}");
        var inDir = Path.Combine("/tmp", $"sandbox-in-{runId}");

        Directory.CreateDirectory(runDir);
        Directory.CreateDirectory(outDir);
        Directory.CreateDirectory(inDir);

        // Make output directory writable by sandbox container user (UID 10001)
        try
        {
            var chmodPsi = new ProcessStartInfo
            {
                FileName = "chmod",
                Arguments = $"777 {outDir}",
                UseShellExecute = false,
                CreateNoWindow = true
            };
            using var chmodProcess = Process.Start(chmodPsi);
            if (chmodProcess != null) await chmodProcess.WaitForExitAsync();
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[Dotnet Worker] Warning setting permissions on outDir: {ex.Message}");
        }

        if (req.InputFiles != null)
        {
            foreach (var fileKvp in req.InputFiles)
            {
                var safeName = Path.GetFileName(fileKvp.Key);
                try
                {
                    var fileBytes = Convert.FromBase64String(fileKvp.Value.Trim());
                    await File.WriteAllBytesAsync(Path.Combine(inDir, safeName), fileBytes);
                }
                catch (Exception ex)
                {
                    Console.WriteLine($"[Dotnet Worker] Error writing input file {safeName}: {ex.Message}");
                }
            }
        }

        var pyFilePath = Path.Combine(runDir, "run.py");
        await File.WriteAllTextAsync(pyFilePath, req.Code);

        var cpusVal = ClampCpus(req.Cpus);
        var memVal = ClampMemory(req.MemoryMb);
        var containerName = $"sandbox-dotnet-{runId}";

        var argsList = new List<string>
        {
            "run", "--rm",
            "--name", containerName,
            "--cap-add=NET_ADMIN",
            "--cap-add=NET_RAW",
            "--cap-add=SYS_ADMIN",
            "--device", "/dev/net/tun",
            "--security-opt", "label=disable",
            $"--cpus={cpusVal.ToString(System.Globalization.CultureInfo.InvariantCulture)}",
            $"--memory={memVal}m",
            $"--pids-limit={pidsLimit}",
            $"--timeout={timeoutSec}"
        };

        if (!string.IsNullOrEmpty(seccompProfile))
        {
            argsList.Add("--security-opt");
            argsList.Add($"seccomp={seccompProfile}");
        }

        // Force network to offline for compliance
        argsList.Add("-e");
        argsList.Add("NETWORK_MODE=offline");
        argsList.Add("-v");
        argsList.Add($"{pyFilePath}:/sandbox/run.py:ro");
        argsList.Add("-v");
        argsList.Add($"{outDir}:/output:rw");
        argsList.Add("-v");
        argsList.Add($"{inDir}:/input:ro");
        argsList.Add("run-python-sandbox");

        var podmanPsi = new ProcessStartInfo
        {
            FileName = "podman",
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
            CreateNoWindow = true
        };
        foreach (var arg in argsList)
        {
            podmanPsi.ArgumentList.Add(arg);
        }

        Console.WriteLine($"[Dotnet Worker] Spawning container (cpus={cpusVal} mem={memVal}MB timeout={timeoutSec}s)...");
        var stopwatch = Stopwatch.StartNew();

        using var podmanProcess = Process.Start(podmanPsi);
        if (podmanProcess == null)
        {
            return Results.StatusCode(StatusCodes.Status500InternalServerError);
        }

        var stdoutTask = ReadCappedAsync(podmanProcess.StandardOutput, maxStreamChars);
        var stderrTask = ReadCappedAsync(podmanProcess.StandardError, maxStreamChars);

        // Backstop deadline in case the podman CLI wedges; podman --timeout should
        // stop the container first.
        var timedOut = false;
        using (var cts = new CancellationTokenSource(TimeSpan.FromSeconds(timeoutSec + 15)))
        {
            try
            {
                await podmanProcess.WaitForExitAsync(cts.Token);
            }
            catch (OperationCanceledException)
            {
                timedOut = true;
                try { podmanProcess.Kill(true); } catch { }
                try
                {
                    var rmPsi = new ProcessStartInfo { FileName = "podman", UseShellExecute = false, CreateNoWindow = true };
                    rmPsi.ArgumentList.Add("rm");
                    rmPsi.ArgumentList.Add("-f");
                    rmPsi.ArgumentList.Add(containerName);
                    using var rm = Process.Start(rmPsi);
                    if (rm != null) await rm.WaitForExitAsync();
                }
                catch { }
            }
        }
        stopwatch.Stop();

        var elapsedMs = stopwatch.ElapsedMilliseconds;
        var stdout = await stdoutTask;
        var stderr = await stderrTask;
        var exitCode = timedOut ? 124 : podmanProcess.ExitCode;

        // Read and parse internal metrics if generated
        long maxMemory = 0;
        string cpuPct = "0%";
        double userTime = 0.0;
        double sysTime = 0.0;
        long fsIn = 0;
        long fsOut = 0;
        long volCs = 0;
        long involCs = 0;

        var metricsPath = Path.Combine(outDir, "metrics.json");
        if (File.Exists(metricsPath))
        {
            try
            {
                var content = await File.ReadAllTextAsync(metricsPath);
                var inner = JsonSerializer.Deserialize<InnerMetrics>(content);
                if (inner != null)
                {
                    maxMemory = inner.MaxMemoryKb;
                    cpuPct = inner.CpuPercentage;
                    userTime = inner.UserTimeSec;
                    sysTime = inner.SysTimeSec;
                    fsIn = inner.FsInputs;
                    fsOut = inner.FsOutputs;
                    volCs = inner.VoluntaryContextSwitches;
                    involCs = inner.InvoluntaryContextSwitches;
                }
            }
            catch (Exception ex)
            {
                Console.WriteLine($"[Dotnet Worker] Error reading metrics: {ex.Message}");
            }
            try { File.Delete(metricsPath); } catch { }
        }

        var metrics = new Metrics(elapsedMs, maxMemory, cpuPct, userTime, sysTime, fsIn, fsOut, volCs, involCs);

        // Read output files up to a total byte budget, skipping symlinks so
        // sandboxed code cannot point us at arbitrary host files.
        var outputFiles = new Dictionary<string, string>();
        long outputBudget = maxOutputBytes;
        try
        {
            foreach (var file in Directory.GetFiles(outDir))
            {
                var name = Path.GetFileName(file);
                if (name == "metrics.json") continue;
                var fi = new FileInfo(file);
                if ((fi.Attributes & FileAttributes.ReparsePoint) != 0) continue;
                if (fi.Length > outputBudget)
                {
                    Console.WriteLine($"[Dotnet Worker] Skipping output file {name}: exceeds remaining output budget");
                    continue;
                }
                var fileBytes = await File.ReadAllBytesAsync(file);
                outputBudget -= fileBytes.Length;
                outputFiles[name] = Convert.ToBase64String(fileBytes);
            }
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[Dotnet Worker] Error reading output files: {ex.Message}");
        }

        // Clean up temporary run directories
        try { Directory.Delete(runDir, true); } catch { }
        try { Directory.Delete(outDir, true); } catch { }
        try { Directory.Delete(inDir, true); } catch { }

        var response = new RunResponse(stdout, stderr, exitCode, metrics, outputFiles, runId, timedOut);
        return Results.Ok(response);
    }
    finally
    {
        runSemaphore.Release();
    }
});

app.Run();

// Helper class for metrics.json deserialization
public class InnerMetrics
{
    [JsonPropertyName("max_memory_kb")]
    public long MaxMemoryKb { get; set; }

    [JsonPropertyName("cpu_percentage")]
    public string CpuPercentage { get; set; } = "0%";

    [JsonPropertyName("user_time_sec")]
    public double UserTimeSec { get; set; }

    [JsonPropertyName("sys_time_sec")]
    public double SysTimeSec { get; set; }

    [JsonPropertyName("fs_inputs")]
    public long FsInputs { get; set; }

    [JsonPropertyName("fs_outputs")]
    public long FsOutputs { get; set; }

    [JsonPropertyName("voluntary_context_switches")]
    public long VoluntaryContextSwitches { get; set; }

    [JsonPropertyName("involuntary_context_switches")]
    public long InvoluntaryContextSwitches { get; set; }
}

// Request and Response records
public record RunRequest(
    string Code,
    string? Network,
    double? Cpus,
    long? MemoryMb,
    Dictionary<string, string>? InputFiles
);
public record Metrics(
    long WallTimeMs,
    long MaxMemoryKb,
    string CpuPercentage,
    double UserTimeSec,
    double SysTimeSec,
    long FsInputs,
    long FsOutputs,
    long VoluntaryContextSwitches,
    long InvoluntaryContextSwitches
);
public record RunResponse(
    string Stdout,
    string Stderr,
    int ExitCode,
    Metrics Metrics,
    Dictionary<string, string> OutputFiles,
    string RunId,
    bool TimedOut
);
