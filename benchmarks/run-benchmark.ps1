param(
    [int]$Iterations = 1000000,
    [int]$Runs = 7,
    [int[]]$Vus = @(32, 64, 128, 256, 512),
    [string]$Version = "v0.1.0-local"
)

$ErrorActionPreference = "Stop"
$compose = Join-Path $PSScriptRoot "docker-compose.yml"
$resultRoot = Join-Path $PSScriptRoot "results\$Version"
New-Item -ItemType Directory -Force -Path $resultRoot | Out-Null

docker compose -f $compose build --no-cache
if ($LASTEXITCODE -ne 0) { throw "Docker image build failed" }

$records = [System.Collections.Generic.List[object]]::new()
foreach ($vu in $Vus) {
    for ($run = 1; $run -le $Runs; $run++) {
        $order = @("raw", "oas") | Get-Random -Count 2
        foreach ($implementation in $order) {
            $service = $implementation
            $file = Join-Path $resultRoot "$implementation-vu$vu-run$run.json"
            $targetUrl = "http://{0}:8080" -f $service
            $env:TARGET_URL = $targetUrl
            $env:VUS = "$vu"
            $env:ITERATIONS = "$Iterations"
            $env:WARMUP_SECONDS = "30"
            $env:RESULT_FILE = "/results/$implementation-vu$vu-run$run.json"
            docker compose -f $compose down --remove-orphans | Out-Null
            docker compose -f $compose up -d $service
            if ($LASTEXITCODE -ne 0) { throw "Failed to start $service" }
            docker compose -f $compose run --rm --no-deps `
                -e TARGET_URL="$targetUrl" `
                -e VUS="$vu" `
                -e ITERATIONS="$Iterations" `
                -e WARMUP_SECONDS="30" `
                -e MODE="warmup" `
                k6 run /bench/k6.js
            if ($LASTEXITCODE -ne 0) { throw "Warmup failed for $implementation vu=$vu run=$run" }
            docker compose -f $compose run --rm --no-deps `
                -e TARGET_URL="$targetUrl" `
                -e VUS="$vu" `
                -e ITERATIONS="$Iterations" `
                -e MODE="measured" `
                -e RESULT_FILE="/results/$implementation-vu$vu-run$run.json" `
                k6 run /bench/k6.js --summary-export $env:RESULT_FILE
            $exitCode = $LASTEXITCODE
            $sharedFile = Join-Path $PSScriptRoot "results\$implementation-vu$vu-run$run.json"
            if (Test-Path $sharedFile) { Move-Item -Force $sharedFile $file }
            docker compose -f $compose down --remove-orphans | Out-Null
            if ($exitCode -ne 0) { throw "Benchmark failed for $implementation vu=$vu run=$run" }
            $records.Add([pscustomobject]@{ implementation = $implementation; vu = $vu; run = $run; file = $file })
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

$reportRows = foreach ($vu in $Vus) {
    $rawMetrics = @($records | Where-Object { $_.implementation -eq "raw" -and $_.vu -eq $vu } | ForEach-Object { Get-Content -Raw $_.file | ConvertFrom-Json })
    $oasMetrics = @($records | Where-Object { $_.implementation -eq "oas" -and $_.vu -eq $vu } | ForEach-Object { Get-Content -Raw $_.file | ConvertFrom-Json })
    if ($rawMetrics.Count -eq 0 -or $oasMetrics.Count -eq 0) { continue }
    $rawRps = Get-Median @($rawMetrics | ForEach-Object { [double]$_.metrics.measured_requests.rate })
    $oasRps = Get-Median @($oasMetrics | ForEach-Object { [double]$_.metrics.measured_requests.rate })
    $rawP99 = Get-Median @($rawMetrics | ForEach-Object { [double]$_.metrics.measured_http_req_duration.'p(99)' })
    $oasP99 = Get-Median @($oasMetrics | ForEach-Object { [double]$_.metrics.measured_http_req_duration.'p(99)' })
    [pscustomobject]@{
        Vus = $vu
        RawRps = $rawRps
        OasRps = $oasRps
        Overhead = 1 - ($oasRps / $rawRps)
        RawP99 = $rawP99
        OasP99 = $oasP99
        P99Delta = ($oasP99 / $rawP99) - 1
    }
}
$table = if ($reportRows) {
    ($reportRows | ForEach-Object { "| plaintext (VU $($_.Vus)) | $([math]::Round($_.RawRps, 2)) | $([math]::Round($_.OasRps, 2)) | $([math]::Round($_.Overhead * 100, 3))% | $([math]::Round($_.RawP99, 4))ms | $([math]::Round($_.OasP99, 4))ms | n/a | INCONCLUSIVE |" }) -join "`n"
} else {
    "| plaintext | pending aggregation | pending aggregation | pending | pending | pending | pending | INCONCLUSIVE |"
}

@"
# Benchmark report $Version

Status: INCONCLUSIVE

This run collected $($records.Count) raw/framework measurements. The report
remains INCONCLUSIVE until raw-baseline coefficient of variation and paired
95% confidence intervals are calculated on the dedicated benchmark host.

| Test | Raw RPS | OAS RPS | Overhead | Raw p99 | OAS p99 | CPU Δ | Result |
|---|---:|---:|---:|---:|---:|---:|---|
${table}
| static JSON | not collected in smoke script | not collected in smoke script | pending | pending | pending | pending | INCONCLUSIVE |

Raw result files and the exact environment must be retained beside this file.
"@ | Set-Content -Path (Join-Path $resultRoot "REPORT.md")

Get-ComputerInfo | Out-File (Join-Path $resultRoot "environment.md")
docker version | Out-File (Join-Path $resultRoot "docker-version.txt")
rustc --version | Out-File (Join-Path $resultRoot "rust-version.txt")
Write-Host "Benchmark artifacts written to $resultRoot"
