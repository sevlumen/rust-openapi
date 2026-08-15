[CmdletBinding()]
param(
    [string[]]$Cases = @("users", "users-static"),
    [ValidateSet("raw", "oas")]
    [string[]]$Implementations = @("raw", "oas"),
    [ValidateRange(1, 100)]
    [int]$Runs = 3,
    [ValidateRange(1, 1000000000)]
    [int]$Requests = 1000000,
    [ValidateRange(1, 4096)]
    [int]$Connections = 256,
    [ValidateRange(0, 10000000)]
    [int]$WarmupRequests = 10000,
    [ValidateRange(1024, 65535)]
    [int]$Port = 18080,
    [string]$Version = "native-diagnostic"
)

$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$serverPath = Join-Path $repoRoot "target\release\oas-bench-server.exe"
$ohaCommand = Get-Command oha -ErrorAction SilentlyContinue
if (-not $ohaCommand) {
    throw "oha.exe was not found on PATH. Install a native oha binary before running this harness."
}

if (-not (Test-Path -LiteralPath $serverPath)) {
    cargo build --release --bin oas-bench-server
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed with exit code $LASTEXITCODE"
    }
}

$commit = (& git -C $repoRoot rev-parse --short HEAD).Trim()
$timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
$resultRoot = Join-Path $repoRoot "benchmarks\results\$Version-$commit-$timestamp"
New-Item -ItemType Directory -Force -Path $resultRoot | Out-Null

$records = [System.Collections.Generic.List[object]]::new()

function Wait-NativeServer {
    param([int]$ServerPort, [System.Diagnostics.Process]$Process)

    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($Process.HasExited) {
            throw "benchmark server exited before it became ready (exit code $($Process.ExitCode))"
        }
        $client = [Net.Sockets.TcpClient]::new()
        try {
            $task = $client.ConnectAsync("127.0.0.1", $ServerPort)
            $connected = $task.Wait(250)
            if ($connected -and $client.Connected) {
                return
            }
        } catch {
            # The listener is not ready yet.
        } finally {
            $client.Dispose()
        }
        Start-Sleep -Milliseconds 100
    }
    throw "benchmark server did not listen on 127.0.0.1:$ServerPort within 15 seconds"
}

function Start-NativeServer {
    param([string]$Implementation, [int]$ServerPort, [string]$OutputRoot)

    $stdoutPath = Join-Path $OutputRoot "$Implementation-server.stdout.log"
    $stderrPath = Join-Path $OutputRoot "$Implementation-server.stderr.log"
    $env:OAS_IMPLEMENTATION = $Implementation
    $env:OAS_LISTEN = "127.0.0.1:$ServerPort"
    $process = Start-Process `
        -FilePath $serverPath `
        -WorkingDirectory $repoRoot `
        -WindowStyle Hidden `
        -RedirectStandardOutput $stdoutPath `
        -RedirectStandardError $stderrPath `
        -PassThru
    Wait-NativeServer -ServerPort $ServerPort -Process $process
    return $process
}

function Stop-NativeServer {
    param([System.Diagnostics.Process]$Process)

    if ($Process -and -not $Process.HasExited) {
        Stop-Process -Id $Process.Id -Force
        $Process.WaitForExit(5000) | Out-Null
    }
}

function Invoke-OhaJson {
    param(
        [string]$Implementation,
        [string]$Case,
        [int]$Run,
        [int]$RequestCount,
        [int]$ConnectionCount,
        [int]$ServerPort,
        [string]$OutputRoot,
        [switch]$Warmup
    )

    $url = "http://127.0.0.1:$ServerPort/$Case"
    if ($Warmup) {
        & $ohaCommand.Source -n $RequestCount -c $ConnectionCount --no-tui --output-format quiet $url | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw "oha warm-up failed for ${Implementation}/${Case}: $LASTEXITCODE"
        }
        return
    }

    $outputPath = Join-Path $OutputRoot "$Implementation-$Case-run$Run.json"
    & $ohaCommand.Source `
        -n $RequestCount `
        -c $ConnectionCount `
        --ipv4 `
        --no-tui `
        --output-format json `
        --output $outputPath `
        $url
    if ($LASTEXITCODE -ne 0) {
        throw "oha failed for ${Implementation}/${Case} run ${Run}: $LASTEXITCODE"
    }

    $summary = Get-Content -Raw -LiteralPath $outputPath | ConvertFrom-Json
    $responseCount = 0
    foreach ($property in $summary.statusCodeDistribution.PSObject.Properties) {
        $responseCount += [int]$property.Value
    }
    if ($responseCount -ne $RequestCount) {
        throw "oha returned $responseCount responses, expected $RequestCount for ${Implementation}/${Case} run ${Run}"
    }
    if ([double]$summary.summary.successRate -ne 1.0) {
        throw "oha reported a non-zero error rate for ${Implementation}/${Case} run ${Run}"
    }

    return [pscustomobject]@{
        case = $Case
        implementation = $Implementation
        run = $Run
        requests = $responseCount
        rps = [double]$summary.summary.requestsPerSec
        total_seconds = [double]$summary.summary.total
        success_rate = [double]$summary.summary.successRate
        p50_ms = [double]$summary.metrics.latency_ms.p50
        p95_ms = [double]$summary.metrics.latency_ms.p95
        p99_ms = [double]$summary.metrics.latency_ms.p99
        output = Split-Path -Leaf $outputPath
    }
}

for ($run = 1; $run -le $Runs; $run++) {
    for ($caseIndex = 0; $caseIndex -lt $Cases.Count; $caseIndex++) {
        $case = $Cases[$caseIndex]
        $order = if ((($run + $caseIndex) % 2) -eq 0) {
            @("raw", "oas")
        } else {
            @("oas", "raw")
        }

        foreach ($implementation in $order) {
            if ($Implementations -notcontains $implementation) {
                continue
            }
            $server = $null
            try {
                $server = Start-NativeServer -Implementation $implementation -ServerPort $Port -OutputRoot $resultRoot
                if ($WarmupRequests -gt 0) {
                    Invoke-OhaJson `
                        -Implementation $implementation `
                        -Case $case `
                        -Run $run `
                        -RequestCount $WarmupRequests `
                        -ConnectionCount $Connections `
                        -ServerPort $Port `
                        -OutputRoot $resultRoot `
                        -Warmup
                }
                $record = Invoke-OhaJson `
                    -Implementation $implementation `
                    -Case $case `
                    -Run $run `
                    -RequestCount $Requests `
                    -ConnectionCount $Connections `
                    -ServerPort $Port `
                    -OutputRoot $resultRoot
                $records.Add($record)
                $record | ConvertTo-Json -Compress | Add-Content -LiteralPath (Join-Path $resultRoot "records.jsonl")
                Write-Host ("{0} {1} run={2} rps={3:N2} p50_ms={4:N3} p95_ms={5:N3} p99_ms={6:N3}" -f `
                    $implementation, $case, $run, $record.rps, $record.p50_ms, $record.p95_ms, $record.p99_ms)
            } finally {
                Stop-NativeServer -Process $server
            }
        }
    }
}

$reportLines = [System.Collections.Generic.List[string]]::new()
$reportLines.Add("# Native benchmark $Version")
$reportLines.Add("")
$reportLines.Add("Diagnostic native Windows run; this is not the dedicated Linux release gate.")
$reportLines.Add("")
$reportLines.Add("| Case | Raw median RPS | OAS median RPS | Throughput delta |")
$reportLines.Add("| --- | ---: | ---: | ---: |")
foreach ($case in $Cases) {
    $raw = @($records | Where-Object { $_.case -eq $case -and $_.implementation -eq "raw" } | Select-Object -ExpandProperty rps | Sort-Object)
    $oas = @($records | Where-Object { $_.case -eq $case -and $_.implementation -eq "oas" } | Select-Object -ExpandProperty rps | Sort-Object)
    if ($raw.Count -eq 0 -or $oas.Count -eq 0) {
        continue
    }
    $rawMedian = $raw[[int][math]::Floor($raw.Count / 2)]
    $oasMedian = $oas[[int][math]::Floor($oas.Count / 2)]
    $delta = (($rawMedian / $oasMedian) - 1.0) * 100.0
    $reportLines.Add("| $case | $([math]::Round($rawMedian, 2)) | $([math]::Round($oasMedian, 2)) | $([math]::Round($delta, 3))% |")
}
$reportLines.Add("")
$reportLines.Add("All measured requests must have the expected HTTP status and success rate 1.0; otherwise the script fails.")
$reportLines | Set-Content -LiteralPath (Join-Path $resultRoot "REPORT.md")

$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
$environment = @(
    "commit=$commit",
    "timestamp=$timestamp",
    "os=$([Environment]::OSVersion.VersionString)",
    "processor=$($cpu.Name)",
    "logical_processors=$([Environment]::ProcessorCount)",
    "oha=$((& $ohaCommand.Source --version) -join ' ')",
    "requests=$Requests",
    "connections=$Connections",
    "warmup_requests=$WarmupRequests",
    "runs=$Runs",
    "cases=$($Cases -join ',')",
    "implementations=$($Implementations -join ',')"
)
$environment | Set-Content -LiteralPath (Join-Path $resultRoot "environment.md")

Get-ChildItem -LiteralPath $resultRoot -File |
    Sort-Object Name |
    ForEach-Object { $_.Name } |
    Set-Content -LiteralPath (Join-Path $resultRoot "manifest.txt")

Write-Host "Native benchmark artifacts written to $resultRoot"
