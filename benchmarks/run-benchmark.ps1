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
            docker compose -f $compose run --rm --no-deps `
                -e TARGET_URL="$targetUrl" `
                -e VUS="$vu" `
                -e ITERATIONS="$Iterations" `
                -e CASE="$case" `
                -e MODE="measured" `
                -e RESULT_FILE="/results/$case-$implementation-vu$vu-run$run.json" `
                k6 run /bench/k6.js --summary-export $env:RESULT_FILE
            $exitCode = $LASTEXITCODE
            $sharedFile = Join-Path $PSScriptRoot "results\$case-$implementation-vu$vu-run$run.json"
            if (Test-Path $sharedFile) { Move-Item -Force $sharedFile $file }
            docker compose -f $compose down --remove-orphans | Out-Null
            if ($exitCode -ne 0) { throw "Benchmark failed for $implementation vu=$vu run=$run" }
            $records.Add([pscustomobject]@{ case = $case; implementation = $implementation; vu = $vu; run = $run; file = $file })
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

$reportRows = foreach ($case in $Cases) { foreach ($vu in $Vus) {
    $rawRecords = @($records | Where-Object { $_.case -eq $case -and $_.implementation -eq "raw" -and $_.vu -eq $vu } | Sort-Object run)
    $oasRecords = @($records | Where-Object { $_.case -eq $case -and $_.implementation -eq "oas" -and $_.vu -eq $vu } | Sort-Object run)
    $rawMetrics = @($rawRecords | ForEach-Object { Get-Content -Raw $_.file | ConvertFrom-Json })
    $oasMetrics = @($oasRecords | ForEach-Object { Get-Content -Raw $_.file | ConvertFrom-Json })
    if ($rawMetrics.Count -eq 0 -or $oasMetrics.Count -eq 0) { continue }
    $rawRps = Get-Median @($rawMetrics | ForEach-Object { [double]$_.metrics.measured_requests.rate })
    $oasRps = Get-Median @($oasMetrics | ForEach-Object { [double]$_.metrics.measured_requests.rate })
    $rawP99 = Get-Median @($rawMetrics | ForEach-Object { [double]$_.metrics.measured_http_req_duration.'p(99)' })
    $oasP99 = Get-Median @($oasMetrics | ForEach-Object { [double]$_.metrics.measured_http_req_duration.'p(99)' })
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
    $result = if ($errors -gt 0 -or $Runs -lt 7 -or $Iterations -lt 1000000 -or $rawCv -gt 0.005) { "INCONCLUSIVE" } elseif ($medianOverhead -gt 0.01 -or $ciUpper -gt 0.01 -or (($oasP99 / $rawP99) - 1) -gt 0.01) { "FAIL" } else { "PASS" }
    [pscustomobject]@{
        Case = $case
        Vus = $vu
        RawRps = $rawRps
        OasRps = $oasRps
        Overhead = 1 - ($oasRps / $rawRps)
        RawP99 = $rawP99
        OasP99 = $oasP99
        P99Delta = ($oasP99 / $rawP99) - 1
        RawCv = $rawCv
        MedianOverhead = $medianOverhead
        Upper95Ci = $ciUpper
        Result = $result
    }
}}
$table = if ($reportRows) {
    ($reportRows | ForEach-Object { "| $($_.Case) (VU $($_.Vus)) | $([math]::Round($_.RawRps, 2)) | $([math]::Round($_.OasRps, 2)) | $([math]::Round($_.Overhead * 100, 3))% | $([math]::Round($_.RawP99, 4))ms | $([math]::Round($_.OasP99, 4))ms | n/a | $($_.Result) |" }) -join "`n"
} else {
    "| no completed cases | pending aggregation | pending aggregation | pending | pending | pending | pending | INCONCLUSIVE |"
}

 $overall = if ($reportRows.Result -contains "FAIL") { "FAIL" } elseif ($reportRows.Count -gt 0 -and @($reportRows.Result | Where-Object { $_ -ne "PASS" }).Count -eq 0) { "PASS" } else { "INCONCLUSIVE" }

@"
# Benchmark report $Version

Status: $overall

This run collected $($records.Count) raw/framework measurements. The report
remains INCONCLUSIVE until raw-baseline coefficient of variation and paired
95% confidence intervals are calculated on the dedicated benchmark host.

| Test | Raw RPS | OAS RPS | Overhead | Raw p99 | OAS p99 | CPU Δ | Result |
|---|---:|---:|---:|---:|---:|---:|---|
${table}

The statistical gate uses median paired throughput overhead, upper normal-approximation 95% CI, p99 delta, raw baseline CV, zero measured errors, and the requested run/request minimums. CPU/RSS/allocation metrics require the dedicated-host collectors and remain separate gates.

Raw result files and the exact environment must be retained beside this file.
"@ | Set-Content -Path (Join-Path $resultRoot "REPORT.md")

Get-ComputerInfo | Out-File (Join-Path $resultRoot "environment.md")
docker version | Out-File (Join-Path $resultRoot "docker-version.txt")
rustc --version | Out-File (Join-Path $resultRoot "rust-version.txt")
Write-Host "Benchmark artifacts written to $resultRoot"
