using System.Diagnostics;
using System.Text.Json;
using System.Text.Json.Serialization;

var builder = WebApplication.CreateBuilder(args);

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

// Configure the port from environment variable PORT (standard behavior)
var port = Environment.GetEnvironmentVariable("PORT") ?? "8082";
app.Urls.Clear();
app.Urls.Add($"http://0.0.0.0:{port}");

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
    var runId = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
    var tempDir = Path.GetTempPath();
    var runDir = Path.Combine(tempDir, $"sandbox-run-dotnet-{runId}");
    var outDir = Path.Combine(tempDir, $"sandbox-out-dotnet-{runId}");

    Directory.CreateDirectory(runDir);
    Directory.CreateDirectory(outDir);

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

    var pyFilePath = Path.Combine(runDir, "run.py");
    await File.WriteAllTextAsync(pyFilePath, req.Code);

    var argsList = new List<string>
    {
        "run", "--rm",
        "--cap-add=NET_ADMIN",
        "--cap-add=NET_RAW",
        "--cap-add=SYS_ADMIN",
        "--device", "/dev/net/tun",
        "--security-opt", "label=disable"
    };

    if (req.Cpus > 0)
    {
        argsList.Add($"--cpus={req.Cpus}");
    }
    if (req.MemoryMb > 0)
    {
        argsList.Add($"--memory={req.MemoryMb}m");
    }

    // Force network to offline for compliance
    argsList.Add("-e");
    argsList.Add("NETWORK_MODE=offline");
    argsList.Add("-v");
    argsList.Add($"{pyFilePath}:/sandbox/run.py:ro");
    argsList.Add("-v");
    argsList.Add($"{outDir}:/output:rw");
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

    Console.WriteLine($"[Dotnet Worker] Spawning container with network=offline...");
    var stopwatch = Stopwatch.StartNew();
    
    using var podmanProcess = Process.Start(podmanPsi);
    if (podmanProcess == null)
    {
        return Results.StatusCode(StatusCodes.Status500InternalServerError);
    }

    var stdoutTask = podmanProcess.StandardOutput.ReadToEndAsync();
    var stderrTask = podmanProcess.StandardError.ReadToEndAsync();

    await podmanProcess.WaitForExitAsync();
    stopwatch.Stop();

    var elapsedMs = stopwatch.ElapsedMilliseconds;
    var stdout = await stdoutTask;
    var stderr = await stderrTask;
    var exitCode = podmanProcess.ExitCode;

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
        try { File.Delete(metricsPath); } catch {}
    }

    var metrics = new Metrics(elapsedMs, maxMemory, cpuPct, userTime, sysTime, fsIn, fsOut, volCs, involCs);

    // Read output files and convert to base64
    var outputFiles = new Dictionary<string, string>();
    try
    {
        foreach (var file in Directory.GetFiles(outDir))
        {
            var name = Path.GetFileName(file);
            if (name == "metrics.json") continue;
            var fileBytes = await File.ReadAllBytesAsync(file);
            var base64 = Convert.ToBase64String(fileBytes);
            outputFiles[name] = base64;
        }
    }
    catch (Exception ex)
    {
        Console.WriteLine($"[Dotnet Worker] Error reading output files: {ex.Message}");
    }

    // Clean up temporary run directories
    try { Directory.Delete(runDir, true); } catch {}
    try { Directory.Delete(outDir, true); } catch {}

    var response = new RunResponse(stdout, stderr, exitCode, metrics, outputFiles);
    return Results.Ok(response);
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
public record RunRequest(string Code, string? Network, double? Cpus, long? MemoryMb);
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
    Dictionary<string, string> OutputFiles
);
