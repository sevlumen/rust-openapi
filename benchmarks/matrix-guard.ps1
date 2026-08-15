function Get-BenchmarkMatrixKey {
    param(
        [string]$Case,
        [string]$Implementation,
        [int]$Vu,
        [int]$Run
    )

    return "$Case|$Implementation|$Vu|$Run"
}

function Get-BenchmarkMatrixStatus {
    param(
        [object[]]$Records,
        [string[]]$Cases,
        [int[]]$Vus,
        [int]$Runs
    )

    $expected = [System.Collections.Generic.HashSet[string]]::new()
    foreach ($case in $Cases) {
        foreach ($vu in $Vus) {
            for ($run = 1; $run -le $Runs; $run++) {
                foreach ($implementation in @("raw", "oas")) {
                    $expected.Add((Get-BenchmarkMatrixKey $case $implementation $vu $run)) | Out-Null
                }
            }
        }
    }

    $actual = @($Records | ForEach-Object {
        Get-BenchmarkMatrixKey $_.case $_.implementation $_.vu $_.run
    })
    $actualSet = [System.Collections.Generic.HashSet[string]]::new()
    foreach ($key in $actual) {
        $actualSet.Add($key) | Out-Null
    }
    $duplicates = @($actual | Group-Object | Where-Object Count -gt 1 | Select-Object -ExpandProperty Name)
    $missing = @($expected | Where-Object { -not $actualSet.Contains($_) } | Sort-Object)
    $unexpected = @($actualSet | Where-Object { -not $expected.Contains($_) } | Sort-Object)

    [pscustomobject]@{
        IsComplete = $missing.Count -eq 0 -and $duplicates.Count -eq 0 -and $unexpected.Count -eq 0
        ExpectedCount = $expected.Count
        ActualCount = $actual.Count
        Missing = $missing
        Duplicates = $duplicates
        Unexpected = $unexpected
    }
}
