$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "..\oha-adapter.ps1")

$summary = Convert-OhaCsvToSummary `
    -CsvPath (Join-Path $PSScriptRoot "oha-sample.csv") `
    -ExpectedStatus 200

if ($summary.metrics.measured_requests.count -ne 3) { throw "request count mismatch" }
if ($summary.metrics.measured_errors.count -ne 1) { throw "status mismatch was not counted" }
if ($summary.metrics.measured_http_req_duration.med -le 0) { throw "median duration missing" }
if ($summary.metrics.measured_http_req_duration.'p(95)' -le 0) { throw "p95 duration missing" }
if ($summary.metrics.measured_requests.rate -le 0) { throw "request rate missing" }

$jsonSummary = Convert-OhaJsonToSummary `
    -JsonPath (Join-Path $PSScriptRoot "oha-sample.json") `
    -ExpectedStatus 200 `
    -ExpectedRequests 3

if ($jsonSummary.metrics.measured_requests.count -ne 3) { throw "JSON request count mismatch" }
if ($jsonSummary.metrics.measured_errors.count -ne 1) { throw "JSON status mismatch was not counted" }
if ($jsonSummary.summary.success_rate -ne (2 / 3)) { throw "JSON success rate was not derived from status counts" }
if ($jsonSummary.metrics.measured_http_req_duration.med -le 0) { throw "JSON median duration missing" }

$wrongCountRejected = $false
try {
    Convert-OhaJsonToSummary `
        -JsonPath (Join-Path $PSScriptRoot "oha-sample.json") `
        -ExpectedStatus 200 `
        -ExpectedRequests 2 | Out-Null
} catch {
    $wrongCountRejected = $true
}
if (-not $wrongCountRejected) { throw "JSON request-count mismatch was accepted" }

Write-Host "oha adapter test passed"
