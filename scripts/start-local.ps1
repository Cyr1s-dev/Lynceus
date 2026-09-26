param(
    [int]$ApiPort = 8000,
    [int]$ConsolePort = 5173,
    [string]$SqlitePath = ".\data\lynceus.db",
    [switch]$NoInstall,
    [switch]$NoBrowser,
    [switch]$Rebuild
)

$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$Root = Resolve-Path (Join-Path $ScriptDir "..")
$ConsoleDir = Join-Path $Root "frontend"
$DataDir = Join-Path $Root "data"
$LogsDir = Join-Path $DataDir "logs"
$PidsDir = Join-Path $DataDir "pids"

New-Item -ItemType Directory -Force -Path $DataDir, $LogsDir, $PidsDir | Out-Null

function Require-Command {
    param([string]$Name)
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "Missing command '$Name'. Install it and retry."
    }
}

function Test-PortOpen {
    param([int]$Port)
    try {
        $client = New-Object System.Net.Sockets.TcpClient
        $task = $client.ConnectAsync("127.0.0.1", $Port)
        $ok = $task.Wait(250)
        $client.Close()
        return $ok
    } catch {
        return $false
    }
}

function Find-AvailablePort {
    param([int]$StartPort)
    for ($candidate = $StartPort; $candidate -lt ($StartPort + 100); $candidate++) {
        if (-not (Test-PortOpen $candidate)) { return $candidate }
    }
    throw "No available API port found in $StartPort-$($StartPort + 99)."
}

function Wait-ApiHealthy {
    param(
        [int]$Port,
        [int]$TimeoutSeconds = 30
    )
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        try {
            $response = Invoke-WebRequest -Uri "http://127.0.0.1:$Port/health" -TimeoutSec 2 -UseBasicParsing
            if ($response.StatusCode -eq 200) { return $true }
        } catch {
        }
        Start-Sleep -Milliseconds 500
    }
    return $false
}

function Resolve-NormalizedPath {
    param([string]$Path)
    if ([string]::IsNullOrWhiteSpace($Path)) { return $null }
    try {
        return ([System.IO.Path]::GetFullPath($Path)).TrimEnd('\').ToLowerInvariant()
    } catch {
        return $null
    }
}

function Test-PidTreeContainsExecutable {
    param(
        [int]$ProcessId,
        [string]$ExpectedPath
    )
    $expected = Resolve-NormalizedPath $ExpectedPath
    $queue = New-Object System.Collections.Generic.Queue[int]
    $queue.Enqueue($ProcessId)
    while ($queue.Count -gt 0) {
        $current = $queue.Dequeue()
        $process = Get-CimInstance Win32_Process -Filter "ProcessId = $current" `
            -ErrorAction SilentlyContinue
        if (-not $process) { continue }
        if ((Resolve-NormalizedPath $process.ExecutablePath) -eq $expected) { return $true }
        $children = Get-CimInstance Win32_Process -Filter "ParentProcessId = $current" `
            -ErrorAction SilentlyContinue
        foreach ($child in $children) { $queue.Enqueue([int]$child.ProcessId) }
    }
    return $false
}

function Remove-UnownedPidFile {
    param(
        [string]$PidFile,
        [string]$ExpectedExecutable
    )
    if (-not (Test-Path $PidFile)) { return }
    $pidValue = Get-Content $PidFile -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $pidValue) {
        Remove-Item -LiteralPath $PidFile -Force
        return
    }
    $process = Get-Process -Id ([int]$pidValue) -ErrorAction SilentlyContinue
    if (-not $process) {
        Remove-Item -LiteralPath $PidFile -Force
        return
    }
    if (-not (Test-PidTreeContainsExecutable -ProcessId ([int]$pidValue) -ExpectedPath $ExpectedExecutable)) {
        Write-Host "[stale] refusing to stop unowned pid=$pidValue; removing stale PID file only"
        Remove-Item -LiteralPath $PidFile -Force
    }
}

function Start-ManagedProcess {
    param(
        [string]$Name,
        [string]$WorkingDirectory,
        [string]$Command,
        [string]$PidFile,
        [string]$OutFile,
        [string]$ErrFile,
        [int]$ExpectedPort = 0,
        [hashtable]$Environment = @{}
    )

    if (Test-Path $PidFile) {
        $existingPid = Get-Content $PidFile -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($existingPid) {
            $existing = Get-Process -Id ([int]$existingPid) -ErrorAction SilentlyContinue
            if ($existing) {
                if ($ExpectedPort -gt 0 -and -not (Test-PortOpen $ExpectedPort)) {
                    Write-Host "[stale] $Name pid=$existingPid exists but port $ExpectedPort is closed; restarting"
                    Stop-Process -Id ([int]$existingPid) -Force -ErrorAction SilentlyContinue
                    Remove-Item -Path $PidFile -Force -ErrorAction SilentlyContinue
                } else {
                    Write-Host "[skip] $Name already running, pid=$existingPid"
                    return
                }
            }
        }
    }

    $envLines = @()
    foreach ($key in $Environment.Keys) {
        $safeValue = [string]$Environment[$key]
        $escaped = $safeValue.Replace("'", "''")
        $envLines += "`$env:$key = '$escaped'"
    }
    $envScript = $envLines -join "; "
    $outEscaped = $OutFile.Replace("'", "''")
    $errEscaped = $ErrFile.Replace("'", "''")
    $fullCommand = (
        "Set-Location '$WorkingDirectory'; " +
        "$envScript; " +
        "$Command 1>> '$outEscaped' 2>> '$errEscaped'"
    )

    $process = Start-Process `
        -FilePath "powershell.exe" `
        -ArgumentList @("-NoExit", "-ExecutionPolicy", "Bypass", "-Command", $fullCommand) `
        -WorkingDirectory $WorkingDirectory `
        -WindowStyle Hidden `
        -PassThru

    Set-Content -Path $PidFile -Value $process.Id
    Write-Host "[start] $Name pid=$($process.Id)"
    Write-Host "        stdout: $OutFile"
    Write-Host "        stderr: $ErrFile"
}

Require-Command "cargo"
Require-Command "npm"

Set-Location $Root

$apiBinary = Join-Path $Root "build\debug\api.exe"
$apiPidFile = Join-Path $PidsDir "api.pid"
Remove-UnownedPidFile -PidFile $apiPidFile -ExpectedExecutable $apiBinary

$apiPortWasExplicit = $PSBoundParameters.ContainsKey("ApiPort")
$apiAlreadyRunning = Test-PortOpen $ApiPort
$ownedApiRunning = Get-CimInstance Win32_Process -Filter "Name = 'api.exe'" `
    -ErrorAction SilentlyContinue | Where-Object {
        (Resolve-NormalizedPath $_.ExecutablePath) -eq (Resolve-NormalizedPath $apiBinary)
    } | Select-Object -First 1
if ($apiAlreadyRunning -and -not $ownedApiRunning) {
    if ($apiPortWasExplicit) {
        throw "Port $ApiPort is open, but it is not owned by $apiBinary. Choose another -ApiPort."
    }
    $requestedApiPort = $ApiPort
    $ApiPort = Find-AvailablePort ($ApiPort + 1)
    Write-Warning "Port $requestedApiPort is occupied by another process; using API port $ApiPort."
}

if (-not $NoInstall) {
    # API 已在运行且健康时跳过重建：避免把一个正常服务的 dev 栈
    # 无故杀掉（旧行为：无条件 build-local → Stop-Process → 重启）。
    # 需要带上新二进制时显式传 -Rebuild。
    $apiAlreadyRunning = Test-PortOpen $ApiPort
    $ownedApiRunning = Get-CimInstance Win32_Process -Filter "Name = 'api.exe'" `
        -ErrorAction SilentlyContinue | Where-Object {
            (Resolve-NormalizedPath $_.ExecutablePath) -eq (Resolve-NormalizedPath $apiBinary)
        } | Select-Object -First 1
    if ($apiAlreadyRunning -and -not $ownedApiRunning) {
        throw "Port $ApiPort is open, but it is not owned by $apiBinary. Refusing to stop or reuse an unrelated service."
    }
    if ($apiAlreadyRunning -and -not $Rebuild) {
        Write-Host "[skip] API already running on port $ApiPort; not rebuilding (pass -Rebuild to force)."
    } else {
        Write-Host "[setup] building Rust API..."
        # 统一走安全构建生命周期：运行中的 api.exe 先按归属校验停止，
        # 构建完成后由本脚本的 Start-ManagedProcess 负责启动（-NoRestart）。
        & (Join-Path $ScriptDir "build-local.ps1") -NoRestart
        if ($LASTEXITCODE -ne 0) {
            throw "API build failed (build-local.ps1 exit $LASTEXITCODE)."
        }
    }

    if (-not (Test-Path (Join-Path $ConsoleDir "node_modules"))) {
        Write-Host "[setup] installing console dependencies..."
        Push-Location $ConsoleDir
        npm install
        Pop-Location
    } else {
        Write-Host "[setup] console node_modules exists, skip npm install"
    }
}

$sqliteFullPath = if ([System.IO.Path]::IsPathRooted($SqlitePath)) {
    $SqlitePath
} else {
    Join-Path $Root $SqlitePath
}
$sqliteDirectory = Split-Path -Parent $sqliteFullPath
New-Item -ItemType Directory -Force -Path $sqliteDirectory | Out-Null

if (-not (Test-Path -LiteralPath $apiBinary)) {
    throw "Rust API binary not found: $apiBinary. Run cargo build -p api first."
}

$sharedEnv = @{
    "LYNCEUS_DB" = $sqliteFullPath
    "LYNCEUS_BIND" = "127.0.0.1:$ApiPort"
    "VITE_API_BASE_URL" = "http://127.0.0.1:$ApiPort"
    "LYNCEUS_WORKSPACE_DIR" = $DataDir
    "LYNCEUS_FINGERPRINT_PACKS_DIR" = (Join-Path $Root "resources\fingerprints")
    "LYNCEUS_LOCAL_TOOLS_CONFIG" = (Join-Path $DataDir "config\local-tools.json")
}

if (Test-PortOpen $ApiPort) {
    $ownedApiRunning = Get-CimInstance Win32_Process -Filter "Name = 'api.exe'" `
        -ErrorAction SilentlyContinue | Where-Object {
        (Resolve-NormalizedPath $_.ExecutablePath) -eq (Resolve-NormalizedPath $apiBinary)
    } | Select-Object -First 1
    if (-not $ownedApiRunning) {
        throw "Port $ApiPort is open, but it is not owned by $apiBinary."
    }
    Write-Host "[skip] API port $ApiPort is already open: http://127.0.0.1:$ApiPort"
} else {
    Start-ManagedProcess `
        -Name "api" `
        -WorkingDirectory $Root `
        -Command "& '$apiBinary'" `
        -PidFile (Join-Path $PidsDir "api.pid") `
        -OutFile (Join-Path $LogsDir "api.out.log") `
        -ErrFile (Join-Path $LogsDir "api.err.log") `
        -ExpectedPort $ApiPort `
        -Environment $sharedEnv
}

if (-not (Wait-ApiHealthy -Port $ApiPort)) {
    throw "API did not become healthy on port $ApiPort within 30s (see $LogsDir\api.err.log)."
}
Write-Host "[ok] API healthy: http://127.0.0.1:$ApiPort"

# LiteLLM Gateway sidecar 随栈启动：网关不在时 pi/dsh 等引擎的
# anthropic-messages 调用会集体 Connection error（直连上游又 401），
# 而它此前只靠按需 spawn，掉了没人拉。uvx 冷启动首拍要现下载 litellm
# 包，可能撞内部 60s 就绪等待返回 422——重试即可（包缓存后秒起）。
$gatewayRunning = $false
foreach ($attempt in 1..3) {
    try {
        $gateway = Invoke-RestMethod -Uri "http://127.0.0.1:$ApiPort/gateway/status" -TimeoutSec 5
        if ($gateway.running) {
            Write-Host "[skip] LiteLLM gateway already running on port $($gateway.port)"
            $gatewayRunning = $true
            break
        }
        $gateway = Invoke-RestMethod -Uri "http://127.0.0.1:$ApiPort/gateway/start" -Method Post -TimeoutSec 120
        if ($gateway.running) {
            Write-Host "[start] LiteLLM gateway pid=$($gateway.pid) port=$($gateway.port) models=$($gateway.models -join ', ')"
            $gatewayRunning = $true
            break
        }
        Write-Warning "LiteLLM gateway start attempt $attempt failed: $($gateway.last_error)"
    } catch {
        Write-Warning "LiteLLM gateway start attempt $attempt error: $($_.Exception.Message)"
    }
    Start-Sleep -Seconds 2
}
if (-not $gatewayRunning) {
    Write-Warning "LiteLLM gateway is NOT running; pi/dsh workers will fail until it is up. Retry: curl -X POST http://127.0.0.1:$ApiPort/gateway/start"
}

if (Test-PortOpen $ConsolePort) {
    Write-Host "[skip] Console port $ConsolePort is already open: http://127.0.0.1:$ConsolePort"
} else {
    Start-ManagedProcess `
        -Name "lynceus-console" `
        -WorkingDirectory $ConsoleDir `
        -Command "npm run dev -- --host 127.0.0.1 --port $ConsolePort" `
        -PidFile (Join-Path $PidsDir "console.pid") `
        -OutFile (Join-Path $LogsDir "console.out.log") `
        -ErrFile (Join-Path $LogsDir "console.err.log") `
        -ExpectedPort $ConsolePort `
        -Environment $sharedEnv
}

Write-Host ""
Write-Host "Lynceus local stack is starting."
Write-Host "API:     http://127.0.0.1:$ApiPort/health"
Write-Host "Console: http://127.0.0.1:$ConsolePort"
Write-Host "SQLite:  $sqliteFullPath"
Write-Host "Logs:    $LogsDir"
Write-Host ""
Write-Host "Stop with:"
Write-Host "  powershell -ExecutionPolicy Bypass -File .\scripts\stop-local.ps1"

if (-not $NoBrowser) {
    Start-Sleep -Seconds 2
    Start-Process "http://127.0.0.1:$ConsolePort"
}
