$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "..\matrix-guard.ps1")

$cases = @("plaintext", "users-static")
$vus = @(32, 64)
$runs = 2
$completeRecords = @(
    [pscustomobject]@{ case = "plaintext"; implementation = "raw"; vu = 32; run = 1 }
    [pscustomobject]@{ case = "plaintext"; implementation = "oas"; vu = 32; run = 1 }
    [pscustomobject]@{ case = "plaintext"; implementation = "raw"; vu = 32; run = 2 }
    [pscustomobject]@{ case = "plaintext"; implementation = "oas"; vu = 32; run = 2 }
    [pscustomobject]@{ case = "plaintext"; implementation = "raw"; vu = 64; run = 1 }
    [pscustomobject]@{ case = "plaintext"; implementation = "oas"; vu = 64; run = 1 }
    [pscustomobject]@{ case = "plaintext"; implementation = "raw"; vu = 64; run = 2 }
    [pscustomobject]@{ case = "plaintext"; implementation = "oas"; vu = 64; run = 2 }
    [pscustomobject]@{ case = "users-static"; implementation = "raw"; vu = 32; run = 1 }
    [pscustomobject]@{ case = "users-static"; implementation = "oas"; vu = 32; run = 1 }
    [pscustomobject]@{ case = "users-static"; implementation = "raw"; vu = 32; run = 2 }
    [pscustomobject]@{ case = "users-static"; implementation = "oas"; vu = 32; run = 2 }
    [pscustomobject]@{ case = "users-static"; implementation = "raw"; vu = 64; run = 1 }
    [pscustomobject]@{ case = "users-static"; implementation = "oas"; vu = 64; run = 1 }
    [pscustomobject]@{ case = "users-static"; implementation = "raw"; vu = 64; run = 2 }
    [pscustomobject]@{ case = "users-static"; implementation = "oas"; vu = 64; run = 2 }
)

$complete = Get-BenchmarkMatrixStatus -Records $completeRecords -Cases $cases -Vus $vus -Runs $runs
if (-not $complete.IsComplete) {
    throw "complete matrix was rejected: missing=$($complete.Missing -join ',')"
}

$incompleteRecords = @($completeRecords)
$incompleteRecords[0] = $incompleteRecords[1]
$incomplete = Get-BenchmarkMatrixStatus -Records $incompleteRecords -Cases $cases -Vus $vus -Runs $runs
if ($incomplete.IsComplete) {
    throw "duplicate/missing tuple was incorrectly accepted"
}
if ($incomplete.Duplicates.Count -eq 0 -or $incomplete.Missing.Count -eq 0) {
    throw "duplicate/missing tuple details were not reported"
}

$unexpectedRecords = @($completeRecords + [pscustomobject]@{
        case = "unexpected"
        implementation = "raw"
        vu = 32
        run = 1
    })
$unexpected = Get-BenchmarkMatrixStatus `
    -Records $unexpectedRecords `
    -Cases $cases `
    -Vus $vus `
    -Runs $runs
if ($unexpected.IsComplete -or $unexpected.Unexpected.Count -eq 0) {
    throw "unexpected tuple was incorrectly accepted"
}

Write-Host "benchmark matrix guard test passed"
