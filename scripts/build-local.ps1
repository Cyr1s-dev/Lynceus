param(
    # Build profile: debug (default, build\debug\api.exe) or release.
    [ValidateSet("debug", "release")]
    [string]$Profile = "debug",
    # Run cargo test --workspace instead of cargo build -p api.
    [switch]$Test,
    # Extra args forwarded to cargo (overrides the default build/test command).
    [string]$CargoArgs = "",
    # Do not restart a previously running API after a successful build.
    [switch]$NoRestart,
    # Do not restore the previous binary after a failed build.
    [switch]$NoRestoreOnFailure,
    # Print what would happen (ownership verdict + command) without acting.
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$Root = Resolve-Path (Join-Path $ScriptDir "..")
$DataDir = Join-Path $Root "data"
$LogsDir = Join-Path $DataDir "logs"
$PidsDir = Join-Path $DataDir "pids"
$ApiPidFile = Join-Path $PidsDir "api.pid"
$ApiBinary = Join-Path $Root "build\$Profile\api.exe"
$BackupDir = Join-Path $DataDir "build-backup"
$BackupBinary = Join-Path $BackupDir "api-$Profile.exe"

# ---------------------------------------------------------------------------
# Process ownership: only a process whose ExecutablePath equals this repo's
# build\<profile>\api.exe counts as the Lynceus API. Never kill by image
# name (taskkill /IM api.exe /F would hit unrelated same-name binaries).
# ---------------------------------------------------------------------------

function Resolve-NormalizedPath {
    param([string]$Path)
    if ([string]::IsNullOrWhiteSpace($Path)) { return $null }
    try {
        return ([System.IO.Path]::GetFullPath($Path)).TrimEnd('\').ToLowerInvariant()
    } catch {
        return $null
    }
}

$script:ExpectedBinaryPath = Resolve-NormalizedPath $ApiBinary
$script:VerifiedHostPids = @()

function Test-IsLynceusApiProcess {
    param([int]$ProcessId)
    $cim = Get-CimInstance Win32_Process -Filter "ProcessId = $ProcessId" `
        -ErrorAction SilentlyContinue
    if (-not $cim -or -not $cim.ExecutablePath) { return $false }
    return (Resolve-NormalizedPath $cim.ExecutablePath) -eq $script:ExpectedBinaryPath
}

function Find-LynceusApiProcesses {
    # All PIDs that own the service (pid-file subtree + global path fallback).
    $owned = @{}

    # 1) pid file: start-local.ps1 records the hosting powershell PID;
    #    api.exe is a descendant, so expand the tree looking for owners.
    if (Test-Path $ApiPidFile) {
        $pidValue = Get-Content $ApiPidFile -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($pidValue -and (Get-Process -Id ([int]$pidValue) -ErrorAction SilentlyContinue)) {
            $hostPid = [int]$pidValue
            $hostOwnsApi = $false
            $queue = New-Object System.Collections.Generic.Queue[int]
            $queue.Enqueue($hostPid)
            while ($queue.Count -gt 0) {
                $current = $queue.Dequeue()
                if (Test-IsLynceusApiProcess -ProcessId $current) {
                    $owned[$current] = $true
                    $hostOwnsApi = $true
                }
                $children = Get-CimInstance Win32_Process -Filter "ParentProcessId = $current" `
                    -ErrorAction SilentlyContinue
                foreach ($child in $children) { $queue.Enqueue([int]$child.ProcessId) }
            }
            if ($hostOwnsApi -and -not $owned.ContainsKey($hostPid)) {
                $script:VerifiedHostPids += $hostPid
            }
        }
    }

    # 2) Global fallback: any process whose ExecutablePath is the target
    #    binary (covers Tauri dev spawn, manual cargo run, etc.).
    $candidates = Get-CimInstance Win32_Process -Filter "Name = 'api.exe'" `
        -ErrorAction SilentlyContinue
    foreach ($candidate in $candidates) {
        if (Test-IsLynceusApiProcess -ProcessId ([int]$candidate.ProcessId)) {
            $owned[[int]$candidate.ProcessId] = $true
        }
    }

    return @($owned.Keys)
}

function Stop-LynceusApi {
    param([int[]]$ProcessIds)
    foreach ($procId in $ProcessIds) {
        # The current Axum composition has no shutdown endpoint/signal hook.
        # Stop only the path-verified process; never match by image name.
        Write-Host "[stop] api.exe pid=$procId (ownership verified: $ApiBinary)"
        Stop-Process -Id $procId -ErrorAction SilentlyContinue
    }
    foreach ($procId in $ProcessIds) {
        # Wait for exit and file-lock release (up to 30s).
        $deadline = (Get-Date).AddSeconds(30)
        while ((Get-Process -Id $procId -ErrorAction SilentlyContinue) -and (Get-Date) -lt $deadline) {
            Start-Sleep -Milliseconds 200
        }
        if (Get-Process -Id $procId -ErrorAction SilentlyContinue) {
            throw "api.exe pid=$procId did not exit within 30s; aborting build to avoid corrupt state."
        }
    }
    foreach ($hostPid in $script:VerifiedHostPids) {
        if (Get-Process -Id $hostPid -ErrorAction SilentlyContinue) {
            Write-Host "[stop] hosting powershell pid=$hostPid (verified API ancestor)"
            Stop-Process -Id $hostPid -Force -ErrorAction SilentlyContinue
        }
    }
    # Service is down: the pid file is stale (whoever restarts rewrites it).
    if (Test-Path $ApiPidFile) {
        Remove-Item -LiteralPath $ApiPidFile -Force -ErrorAction SilentlyContinue
    }
}

function Start-LynceusApi {
    # Same hosting style and environment as start-local.ps1 (API only;
    # the console process is not touched here).
    New-Item -ItemType Directory -Force -Path $DataDir, $LogsDir, $PidsDir | Out-Null
    $sqliteFullPath = Join-Path $DataDir "lynceus.db"
    $outFile = Join-Path $LogsDir "api.out.log"
    $errFile = Join-Path $LogsDir "api.err.log"
    $envScript = (
        "`$env:LYNCEUS_DB = '$sqliteFullPath'; " +
        "`$env:LYNCEUS_BIND = '127.0.0.1:8000'; " +
        "`$env:LYNCEUS_WORKSPACE_DIR = '$DataDir'; " +
        "`$env:LYNCEUS_FINGERPRINT_PACKS_DIR = '$(Join-Path $Root "resources\fingerprints")'; " +
        "`$env:LYNCEUS_LOCAL_TOOLS_CONFIG = '$(Join-Path $DataDir "config\local-tools.json")'"
    )
    $binaryEscaped = $ApiBinary.Replace("'", "''")
    $fullCommand = (
        "Set-Location '$Root'; " +
        "$envScript; " +
        "& '$binaryEscaped' 1>> '$outFile' 2>> '$errFile'"
    )
    $process = Start-Process `
        -FilePath "powershell.exe" `
        -ArgumentList @("-NoExit", "-ExecutionPolicy", "Bypass", "-Command", $fullCommand) `
        -WorkingDirectory $Root `
        -WindowStyle Hidden `
        -PassThru
    Set-Content -Path $ApiPidFile -Value $process.Id
    Write-Host "[start] api pid=$($process.Id) ($ApiBinary)"
}

# ---------------------------------------------------------------------------
# Main flow: detect -> (stop) -> build/test -> restart/restore
# ---------------------------------------------------------------------------

Set-Location $Root

$ownedPids = @(Find-LynceusApiProcesses)
$wasRunning = $ownedPids.Count -gt 0
$hadPreviousBinary = Test-Path -LiteralPath $ApiBinary

if ($wasRunning) {
    Write-Host "[detect] Lynceus API running: pid=$($ownedPids -join ',') ($ApiBinary)"
} else {
    Write-Host "[detect] Lynceus API is not running ($ApiBinary)"
}

$cargoCommand = if ($Test) {
    "cargo test --workspace"
} else {
    $profileArgs = if ($Profile -eq "release") { "--release " } else { "" }
    "cargo build ${profileArgs}-p api"
}
if ($CargoArgs) {
    $cargoCommand = "cargo $CargoArgs"
}

if ($DryRun) {
    Write-Host "[dry-run] was_running=$wasRunning"
    Write-Host "[dry-run] would stop: $($ownedPids -join ',')"
    Write-Host "[dry-run] verified host PIDs: $($script:VerifiedHostPids -join ',')"
    Write-Host "[dry-run] would run: $cargoCommand"
    Write-Host "[dry-run] would restart API: $($wasRunning -and -not $NoRestart)"
    exit 0
}

if ($wasRunning) {
    Stop-LynceusApi -ProcessIds $ownedPids
}

# Keep an explicit recoverable copy. Cargo normally links through a temporary
# artifact, but lifecycle recovery must not depend on that implementation
# detail when the previous service was healthy.
if ($hadPreviousBinary) {
    New-Item -ItemType Directory -Force -Path $BackupDir | Out-Null
    Copy-Item -LiteralPath $ApiBinary -Destination $BackupBinary -Force
    Write-Host "[backup] previous binary: $BackupBinary"
}

$buildOk = $true
Write-Host "[build] $cargoCommand"
$cargoArguments = if ($CargoArgs) {
    @($CargoArgs -split '\s+' | Where-Object { $_ })
} elseif ($Test) {
    @("test", "--workspace")
} elseif ($Profile -eq "release") {
    @("build", "--release", "-p", "api")
} else {
    @("build", "-p", "api")
}
& cargo @cargoArguments
if ($LASTEXITCODE -ne 0) {
    $buildOk = $false
    Write-Host "[fail] cargo exited with code $LASTEXITCODE"
}

if ($buildOk) {
    if (Test-Path -LiteralPath $BackupBinary) {
        Remove-Item -LiteralPath $BackupBinary -Force
    }
    if ($wasRunning -and -not $NoRestart) {
        Start-LynceusApi
    } else {
        Write-Host "[skip] API restart (was_running=$wasRunning, NoRestart=$NoRestart)"
    }
    exit 0
}

# Failure behavior (explicit, never silent): never start a new binary after
# a failed build. Restore the explicit pre-build backup and then restore the
# service state; opt out with -NoRestoreOnFailure.
if (-not $NoRestoreOnFailure -and (Test-Path -LiteralPath $BackupBinary)) {
    Copy-Item -LiteralPath $BackupBinary -Destination $ApiBinary -Force
    Remove-Item -LiteralPath $BackupBinary -Force
    Write-Host "[restore] previous API binary restored"
    if ($wasRunning) {
        Write-Host "[restore] restarting API after failed build"
        Start-LynceusApi
    }
} else {
    Write-Host "[restore] skipped (was_running=$wasRunning, NoRestoreOnFailure=$NoRestoreOnFailure)"
}
exit 1
