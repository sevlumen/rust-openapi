function Get-OhaPercentile([double[]]$values, [double]$percentile) {
    $ordered = @($values | Sort-Object)
    if ($ordered.Count -eq 0) { return $null }
    if ($ordered.Count -eq 1) { return [double]$ordered[0] }
    $rank = ($percentile / 100) * ($ordered.Count - 1)
    $lower = [math]::Floor($rank)
    $upper = [math]::Ceiling($rank)
    if ($lower -eq $upper) { return [double]$ordered[$lower] }
    $weight = $rank - $lower
    return [double]$ordered[$lower] + (([double]$ordered[$upper] - [double]$ordered[$lower]) * $weight)
}

function Convert-OhaCsvToSummary {
    param(
        [Parameter(Mandatory)]
        [string]$CsvPath,
        [Parameter(Mandatory)]
        [int]$ExpectedStatus
    )

    $rows = @(Import-Csv -LiteralPath $CsvPath)
    if ($rows.Count -eq 0) { throw "oha produced no request rows: $CsvPath" }

    $durationsMs = @($rows | ForEach-Object {
        [double]$_.'request-duration' * 1000
    })
    $completionSeconds = @($rows | ForEach-Object {
        [double]$_.'request-start' + [double]$_.'request-duration'
    })
    $totalSeconds = ($completionSeconds | Measure-Object -Maximum).Maximum
    if ($totalSeconds -le 0) { throw "oha reported non-positive elapsed time: $CsvPath" }

    $statuses = @($rows | ForEach-Object { [int]$_.status })
    $errors = @($statuses | Where-Object { $_ -ne $ExpectedStatus }).Count
    $bytes = [int64](($rows | ForEach-Object { [int64]$_.'bytes' } | Measure-Object -Sum).Sum)
    $duration = [pscustomobject]@{
        min = Get-OhaPercentile $durationsMs 0
        avg = ($durationsMs | Measure-Object -Average).Average
        med = Get-OhaPercentile $durationsMs 50
        max = Get-OhaPercentile $durationsMs 100
        'p(95)' = Get-OhaPercentile $durationsMs 95
        'p(99)' = Get-OhaPercentile $durationsMs 99
        'p(99.9)' = Get-OhaPercentile $durationsMs 99.9
    }
    $requestMetric = [pscustomobject]@{
        count = $rows.Count
        rate = $rows.Count / $totalSeconds
    }
    $errorMetric = [pscustomobject]@{
        count = $errors
        rate = $errors / $totalSeconds
    }
    $statusCodes = @{}
    foreach ($status in $statuses) {
        $key = "$status"
        if (-not $statusCodes.ContainsKey($key)) { $statusCodes[$key] = 0 }
        $statusCodes[$key]++
    }

    [pscustomobject]@{
        tool = "oha"
        expected_status = $ExpectedStatus
        metrics = [pscustomobject]@{
            measured_requests = $requestMetric
            measured_errors = $errorMetric
            measured_http_req_duration = $duration
            http_req_duration = $duration
        }
        summary = [pscustomobject]@{
            requests = $rows.Count
            duration_seconds = $totalSeconds
            success_rate = ($rows.Count - $errors) / $rows.Count
            status_codes = $statusCodes
            bytes = $bytes
        }
    }
}
