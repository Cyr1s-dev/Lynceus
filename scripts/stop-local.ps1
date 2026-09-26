param(
    [switch]$KeepLogs
)

$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$Root = Resolve-Path (Join-Path $ScriptDir "..")
$DataDir = Join-Path $Root "data"
$LogsDir = Join-Path $DataDir "logs"
$PidsDir = Join-Path $DataDir "pids"
$ApiBinary = Join-Path $Root "build\debug\api.exe"
$ConsoleDir = Join-Path $Root "frontend"

function Resolve-NormalizedPath {
    param([string]$Path)
    if ([string]::IsNullOrWhiteSpace($Path)) { return $null }
    try {
        return ([System.IO.Path]::GetFullPath($Path)).TrimEnd('\').ToLowerInvariant()
    } catch {
        return $null
    }
}

function Get-ProcessTree {
    param([int]$ProcessId)
    $processes = @()
    $queue = New-Object System.Collections.Generic.Queue[int]
    $queue.Enqueue($ProcessId)
    while ($queue.Count -gt 0) {
        $current = $queue.Dequeue()
        $process = Get-CimInstance Win32_Process -Filter "ProcessId = $current" `
            -ErrorAction SilentlyContinue
        if (-not $process) { continue }
        $processes += $process
        $children = Get-CimInstance Win32_Process -Filter "ParentProcessId = $current" `
            -ErrorAction SilentlyContinue
        foreach ($child in $children) { $queue.Enqueue([int]$child.ProcessId) }
    }
    return @($processes)
}

function Test-ManagedPidOwner {
    param(
        [string]$Name,
        [int]$ProcessId
    )
    $tree = @(Get-ProcessTree -ProcessId $ProcessId)
    if ($Name -eq "api") {
        $expected = Resolve-NormalizedPath $ApiBinary
        return [bool]($tree | Where-Object {
            (Resolve-NormalizedPath $_.ExecutablePath) -eq $expected
        } | Select-Object -First 1)
    }
    $consoleNeedle = (Resolve-NormalizedPath $ConsoleDir).Replace('\', '/')
    return [bool]($tree | Where-Object {
        $commandLine = ([string]$_.CommandLine).Replace('\', '/').ToLowerInvariant()
        $_.Name -eq 'node.exe' -and
        $commandLine.Contains($consoleNeedle) -and
        $commandLine.Contains('vite')
    } | Select-Object -First 1)
}

function Stop-ProcessTree {
    param([int]$ProcessId)

    $children = Get-CimInstance Win32_Process -Filter "ParentProcessId = $ProcessId" `
        -ErrorAction SilentlyContinue
    foreach ($child in $children) {
        Stop-ProcessTree -ProcessId ([int]$child.ProcessId)
    }

    $process = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
    if ($process) {
        Stop-Process -Id $ProcessId -Force
    }
}

if (-not (Test-Path $PidsDir)) {
    Write-Host "No PID directory found: $PidsDir"
    exit 0
}

# LiteLLM gateway sidecar 先于 api 停掉。两条路：
# 1) api 还活着就走 /gateway/stop（Rust 侧树杀 uvx→python 并清状态）；
# 2) api 已死时按 config 路径做归属校验的孤儿清扫——命令行里带本工作区
#    litellm config 的进程树才杀，绝不动别人的 litellm。
try {
    $null = Invoke-RestMethod -Uri "http://127.0.0.1:8000/gateway/stop" -Method Post -TimeoutSec 10
    Write-Host "[stop] LiteLLM gateway via /gateway/stop"
} catch {
}
$gatewayConfigNeedle = "data/gateway/litellm.yaml"
$gatewayOrphans = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object {
    $commandLine = ([string]$_.CommandLine).Replace('\', '/').ToLowerInvariant()
    $commandLine.Contains($gatewayConfigNeedle)
})
foreach ($orphan in $gatewayOrphans) {
    Write-Host "[stop] LiteLLM gateway orphan pid=$($orphan.ProcessId) (config ownership verified)"
    Stop-ProcessTree -ProcessId ([int]$orphan.ProcessId)
}

$pidFiles = @(
    Join-Path $PidsDir "api.pid"
    Join-Path $PidsDir "console.pid"
)

foreach ($pidFile in $pidFiles) {
    if (-not (Test-Path $pidFile)) {
        continue
    }
    $pidValue = Get-Content $pidFile -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $pidValue) {
        Remove-Item -LiteralPath $pidFile -Force
        continue
    }

    $name = [System.IO.Path]::GetFileNameWithoutExtension($pidFile)
    $process = Get-Process -Id ([int]$pidValue) -ErrorAction SilentlyContinue
    if ($process -and (Test-ManagedPidOwner -Name $name -ProcessId ([int]$pidValue))) {
        Write-Host "[stop] $name host pid=$pidValue (ownership verified)"
        Stop-ProcessTree -ProcessId ([int]$pidValue)
    } elseif ($process) {
        Write-Host "[skip] $name pid=$pidValue is not owned by this Lynceus workspace"
    } else {
        Write-Host "[skip] process pid=$pidValue is not running"
    }
    Remove-Item -LiteralPath $pidFile -Force
}

if (-not $KeepLogs -and (Test-Path $LogsDir)) {
    Write-Host "[clean] logs"
    Get-ChildItem -LiteralPath $LogsDir -Filter "*.log" | Remove-Item -Force
}

Write-Host "Lynceus local stack stopped."
