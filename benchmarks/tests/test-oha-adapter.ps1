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

Write-Host "oha adapter test passed"
