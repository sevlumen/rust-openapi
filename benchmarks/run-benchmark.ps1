param(
    [int]$Iterations = 1000000,
    [int]$Runs = 7,
    [int]$WarmupSeconds = 30,
    [int[]]$Vus = @(32, 64, 128, 256, 512),
    [string]$Version = "v0.1.0-local",
    [string[]]$Cases = @("plaintext", "static-json", "static-route", "path-integer", "path-uuid", "query", "header", "json-small", "json-100-users", "postgres", "404", "405")
)

$ErrorActionPreference = "Stop"
$compose = Join-Path $PSScriptRoot "docker-compose.yml"
$resultRoot = Join-Path $PSScriptRoot "results\$Version"
New-Item -ItemType Directory -Force -Path $resultRoot | Out-Null
$Cases = @($Cases | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })

function Start-StatsCollector([string]$service, [string]$file) {
    $container = (docker compose -f $compose ps -q $service).Trim()
    if (-not $container) { return $null }
    "timestamp,cpu_percent,memory_usage,pids" | Set-Content -Path $file
    return Start-Job -ScriptBlock {
        param($containerId, $statsFile)
        while ($true) {
            $sample = docker stats --no-stream --format "{{.CPUPerc}},{{.MemUsage}},{{.PIDs}}" $containerId 2>$null
            if ($sample) {
                "{0},{1}" -f (Get-Date -Format o), $sample | Add-Content -Path $statsFile
            }
            Start-Sleep -Milliseconds 500
        }
    } -ArgumentList $container, $file
}

function Stop-StatsCollector($job) {
    if ($null -ne $job) {
        Stop-Job -Job $job -ErrorAction SilentlyContinue | Out-Null
        Receive-Job -Job $job -ErrorAction SilentlyContinue | Out-Null
        Remove-Job -Job $job -Force -ErrorAction SilentlyContinue
    }
}

docker compose -f $compose build --no-cache
if ($LASTEXITCODE -ne 0) { throw "Docker image build failed" }

$records = [System.Collections.Generic.List[object]]::new()
foreach ($case in $Cases) {
foreach ($vu in $Vus) {
    for ($run = 1; $run -le $Runs; $run++) {
        $order = @("raw", "oas") | Get-Random -Count 2
        foreach ($implementation in $order) {
            $service = $implementation
            $file = Join-Path $resultRoot "$case-$implementation-vu$vu-run$run.json"
            $targetUrl = "http://{0}:8080" -f $service
            $env:TARGET_URL = $targetUrl
            $env:VUS = "$vu"
            $env:ITERATIONS = "$Iterations"
            $env:WARMUP_SECONDS = "$WarmupSeconds"
            $env:CASE = $case
            $env:RESULT_FILE = "/results/$case-$implementation-vu$vu-run$run.json"
            docker compose -f $compose down --remove-orphans | Out-Null
            docker compose -f $compose up -d $service
            if ($LASTEXITCODE -ne 0) { throw "Failed to start $service" }
            docker compose -f $compose run --rm --no-deps `
                -e TARGET_URL="$targetUrl" `
                -e VUS="$vu" `
                -e ITERATIONS="$Iterations" `
                -e WARMUP_SECONDS="$WarmupSeconds" `
                -e CASE="$case" `
                -e MODE="warmup" `
                k6 run /bench/k6.js
            if ($LASTEXITCODE -ne 0) { throw "Warmup failed for $implementation vu=$vu run=$run" }
            $statsFile = Join-Path $resultRoot "$case-$implementation-vu$vu-run$run.stats.csv"
            $statsJob = Start-StatsCollector $service $statsFile
            try {
                docker compose -f $compose run --rm --no-deps `
                    -e TARGET_URL="$targetUrl" `
                    -e VUS="$vu" `
                    -e ITERATIONS="$Iterations" `
                    -e CASE="$case" `
                    -e MODE="measured" `
                    -e RESULT_FILE="/results/$case-$implementation-vu$vu-run$run.json" `
                    k6 run /bench/k6.js --summary-export $env:RESULT_FILE
                $exitCode = $LASTEXITCODE
            } finally {
                Stop-StatsCollector $statsJob
            }
            $sharedFile = Join-Path $PSScriptRoot "results\$case-$implementation-vu$vu-run$run.json"
            if (Test-Path $sharedFile) { Move-Item -Force $sharedFile $file }
            docker compose -f $compose down --remove-orphans | Out-Null
            if ($exitCode -ne 0) { throw "Benchmark failed for $implementation vu=$vu run=$run" }
            $records.Add([pscustomobject]@{ case = $case; implementation = $implementation; vu = $vu; run = $run; file = $file; stats = $statsFile })
        }
    }
}
}

function Get-Median([double[]]$values) {
    $ordered = @($values | Sort-Object)
    if ($ordered.Count -eq 0) { return $null }
    $middle = [math]::Floor($ordered.Count / 2)
    if (($ordered.Count % 2) -eq 1) { return [double]$ordered[$middle] }
    return ([double]$ordered[$middle - 1] + [double]$ordered[$middle]) / 2
}

function Get-StdDev([double[]]$values) {
    if ($values.Count -lt 2) { return 0 }
    $mean = ($values | Measure-Object -Average).Average
    $variance = (($values | ForEach-Object { ($_ - $mean) * ($_ - $mean) } | Measure-Object -Sum).Sum) / ($values.Count - 1)
    return [math]::Sqrt($variance)
}

function Get-MedianMetric($metrics, [string]$name, [string]$property) {
    return Get-Median @($metrics | ForEach-Object { [double]$_.metrics.$name.$property })
}

function Get-AverageCpuPercent($records) {
    $values = @($records | ForEach-Object {
        if (Test-Path $_.stats) {
            Get-Content $_.stats | Select-Object -Skip 1 | ForEach-Object {
                $parts = $_ -split ',', 4
                if ($parts.Count -ge 2) {
                    [double]($parts[1].Trim().TrimEnd('%'))
                }
            }
        }
    })
    if ($values.Count -eq 0) { return $null }
    return ($values | Measure-Object -Average).Average
}

function Get-PeakMemoryMiB($records) {
    $values = @($records | ForEach-Object {
        if (Test-Path $_.stats) {
            Get-Content $_.stats | Select-Object -Skip 1 | ForEach-Object {
                $parts = $_ -split ',', 4
                if ($parts.Count -ge 3 -and $parts[2] -match '^\s*([0-9.]+)(KiB|MiB|GiB|B)') {
                    $number = [double]$matches[1]
                    switch ($matches[2]) {
                        'GiB' { $number *= 1024 }
                        'KiB' { $number /= 1024 }
                        'B' { $number /= 1MB }
                    }
                    $number
                }
            }
        }
    })
    if ($values.Count -eq 0) { return $null }
    return ($values | Measure-Object -Maximum).Maximum
}

$reportRows = foreach ($case in $Cases) { foreach ($vu in $Vus) {
    $rawRecords = @($records | Where-Object { $_.case -eq $case -and $_.implementation -eq "raw" -and $_.vu -eq $vu } | Sort-Object run)
    $oasRecords = @($records | Where-Object { $_.case -eq $case -and $_.implementation -eq "oas" -and $_.vu -eq $vu } | Sort-Object run)
    $rawMetrics = @($rawRecords | ForEach-Object { Get-Content -Raw $_.file | ConvertFrom-Json })
    $oasMetrics = @($oasRecords | ForEach-Object { Get-Content -Raw $_.file | ConvertFrom-Json })
    if ($rawMetrics.Count -eq 0 -or $oasMetrics.Count -eq 0) { continue }
    $rawRps = Get-Median @($rawMetrics | ForEach-Object { [double]$_.metrics.measured_requests.rate })
    $oasRps = Get-Median @($oasMetrics | ForEach-Object { [double]$_.metrics.measured_requests.rate })
    $rawP50 = Get-MedianMetric $rawMetrics measured_http_req_duration med
    $oasP50 = Get-MedianMetric $oasMetrics measured_http_req_duration med
    $rawP95 = Get-MedianMetric $rawMetrics measured_http_req_duration 'p(95)'
    $oasP95 = Get-MedianMetric $oasMetrics measured_http_req_duration 'p(95)'
    $rawP99 = Get-MedianMetric $rawMetrics measured_http_req_duration 'p(99)'
    $oasP99 = Get-MedianMetric $oasMetrics measured_http_req_duration 'p(99)'
    $rawP999 = Get-MedianMetric $rawMetrics measured_http_req_duration 'p(99.9)'
    $oasP999 = Get-MedianMetric $oasMetrics measured_http_req_duration 'p(99.9)'
    $overheads = @()
    for ($index = 0; $index -lt [math]::Min($rawMetrics.Count, $oasMetrics.Count); $index++) {
        $overheads += 1 - ([double]$oasMetrics[$index].metrics.measured_requests.rate / [double]$rawMetrics[$index].metrics.measured_requests.rate)
    }
    $medianOverhead = Get-Median $overheads
    $ciUpper = $medianOverhead + (1.96 * (Get-StdDev $overheads) / [math]::Sqrt($overheads.Count))
    $rawRpsValues = @($rawMetrics | ForEach-Object { [double]$_.metrics.measured_requests.rate })
    $rawMean = ($rawRpsValues | Measure-Object -Average).Average
    $rawCv = if ($rawMean -eq 0) { [double]::PositiveInfinity } else { (Get-StdDev $rawRpsValues) / $rawMean }
    $errors = @($rawMetrics + $oasMetrics | ForEach-Object { [double]$_.metrics.measured_errors.count } | Measure-Object -Sum).Sum
    $rawStatsRecords = @($rawRecords)
    $oasStatsRecords = @($oasRecords)
    $rawCpu = Get-AverageCpuPercent $rawStatsRecords
    $oasCpu = Get-AverageCpuPercent $oasStatsRecords
    $rawMemory = Get-PeakMemoryMiB $rawStatsRecords
    $oasMemory = Get-PeakMemoryMiB $oasStatsRecords
    $cpuDelta = if ($null -eq $rawCpu -or $rawCpu -eq 0 -or $null -eq $oasCpu) { $null } else { ($oasCpu / $rawCpu) - 1 }
    $latencyFail = (($oasP50 / $rawP50) - 1) -gt 0.01 -or (($oasP95 / $rawP95) - 1) -gt 0.01 -or (($oasP99 / $rawP99) - 1) -gt 0.01
    $requiredEvidenceMissing = $null -eq $cpuDelta -or $null -eq $rawMemory -or $null -eq $oasMemory
    $result = if ($errors -gt 0 -or $Runs -lt 7 -or $Iterations -lt 1000000 -or $rawCv -gt 0.005 -or $requiredEvidenceMissing) { "INCONCLUSIVE" } elseif ($medianOverhead -gt 0.01 -or $ciUpper -gt 0.01 -or $latencyFail -or $cpuDelta -gt 0.01) { "FAIL" } else { "PASS" }
    [pscustomobject]@{
        Case = $case
        Vus = $vu
        RawRps = $rawRps
        OasRps = $oasRps
        Overhead = 1 - ($oasRps / $rawRps)
        RawP50 = $rawP50
        OasP50 = $oasP50
        RawP95 = $rawP95
        OasP95 = $oasP95
        RawP99 = $rawP99
        OasP99 = $oasP99
        RawP999 = $rawP999
        OasP999 = $oasP999
        P99Delta = ($oasP99 / $rawP99) - 1
        CpuDelta = $cpuDelta
        RawMemory = $rawMemory
        OasMemory = $oasMemory
        RawCv = $rawCv
        MedianOverhead = $medianOverhead
        Upper95Ci = $ciUpper
        Result = $result
    }
}}
$table = if ($reportRows) {
    ($reportRows | ForEach-Object {
        $cpu = if ($null -eq $_.CpuDelta) { "n/a" } else { "$([math]::Round($_.CpuDelta * 100, 3))%" }
        $rss = if ($null -eq $_.RawMemory -or $null -eq $_.OasMemory) { "n/a" } else { "$([math]::Round($_.RawMemory, 1))/$([math]::Round($_.OasMemory, 1))" }
        "| $($_.Case) (VU $($_.Vus)) | $([math]::Round($_.RawRps, 2)) | $([math]::Round($_.OasRps, 2)) | $([math]::Round($_.Overhead * 100, 3))% | $([math]::Round($_.RawP50, 4))/$([math]::Round($_.OasP50, 4)) | $([math]::Round($_.RawP95, 4))/$([math]::Round($_.OasP95, 4)) | $([math]::Round($_.RawP99, 4))/$([math]::Round($_.OasP99, 4)) | $cpu | $rss | $($_.Result) |"
    }) -join "`n"
} else {
    "| no completed cases | pending aggregation | pending aggregation | pending | pending | pending | pending | pending | pending | INCONCLUSIVE |"
}

 $overall = if ($reportRows.Result -contains "FAIL") { "FAIL" } elseif ($reportRows.Count -gt 0 -and @($reportRows.Result | Where-Object { $_ -ne "PASS" }).Count -eq 0) { "PASS" } else { "INCONCLUSIVE" }

@"
# Benchmark report $Version

Status: $overall

This run collected $($records.Count) raw/framework measurements. Results are
classified only after the minimum run/request counts, raw-baseline CV, paired
95% confidence bound, CPU samples, and RSS samples are available.

| Test | Raw RPS | OAS RPS | Overhead | p50 raw/oas (ms) | p95 raw/oas (ms) | p99 raw/oas (ms) | CPU Δ | RSS peak raw/oas (MiB) | Result |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
${table}

The statistical gate uses median paired throughput overhead, upper normal-approximation 95% CI, p50/p95/p99 latency deltas, raw baseline CV, zero measured errors, CPU samples, memory samples, and the requested run/request minimums. p999 is retained in the JSON artifacts for warning analysis. Allocation metrics are reported by the in-process router benchmark.

Raw result files and the exact environment must be retained beside this file.
"@ | Set-Content -Path (Join-Path $resultRoot "REPORT.md")

$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1 Name, NumberOfCores, NumberOfLogicalProcessors, MaxClockSpeed
$computer = Get-CimInstance Win32_ComputerSystem | Select-Object TotalPhysicalMemory, Manufacturer, Model
$os = Get-CimInstance Win32_OperatingSystem | Select-Object Caption, Version, BuildNumber
$gitSha = (git rev-parse HEAD).Trim()
$rustDetails = rustc -Vv | Out-String
$dockerDetails = docker version | Out-String
$composeVersion = docker compose version
@"
# Benchmark environment

- Git commit: $gitSha
- CPU: $($cpu.Name)
- Physical cores: $($cpu.NumberOfCores)
- Logical processors: $($cpu.NumberOfLogicalProcessors)
- Max clock MHz: $($cpu.MaxClockSpeed)
- Host: $($computer.Manufacturer) $($computer.Model)
- RAM bytes: $($computer.TotalPhysicalMemory)
- OS: $($os.Caption) $($os.Version) build $($os.BuildNumber)
- API CPU affinity: $($env:API_CPUSET) (default 0-3)
- PostgreSQL CPU affinity: $($env:POSTGRES_CPUSET) (default 4-7)
- Load-generator CPU affinity: $($env:LOAD_CPUSET) (default 8-11)
- API/PostgreSQL memory limit: 1g
- Rust compiler:

$rustDetails
- Docker Compose: $composeVersion
- PostgreSQL image: postgres:16.4-bookworm
- k6 image: grafana/k6:0.55.0
- Release profile: opt-level=3, lto=fat, codegen-units=1, panic=abort, strip=true
- Dependencies are locked by Cargo.lock.

Docker version details are retained in docker-version.txt.
"@ | Set-Content (Join-Path $resultRoot "environment.md")
$dockerDetails | Set-Content (Join-Path $resultRoot "docker-version.txt")
$rustDetails | Set-Content (Join-Path $resultRoot "rust-version.txt")
Write-Host "Benchmark artifacts written to $resultRoot"
