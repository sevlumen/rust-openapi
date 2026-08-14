param(
    [int]$Iterations = 1000000,
    [int]$Runs = 7,
    [int]$WarmupSeconds = 30,
    [int[]]$Vus = @(32, 64, 128, 256, 512),
    [string]$Version = "v0.1.0-local",
    [string[]]$Cases = @("plaintext", "static-json", "users-static", "static-route", "path-integer", "path-uuid", "validation-success", "query", "header", "json-small", "json-100-users", "postgres", "problem", "raw-handler", "security", "404", "405"),
    [switch]$ReferenceProfile,
    [switch]$Official,
    [switch]$AllowUndersizedHost,
    [switch]$ContinueAfterInvalidTiming
)

$ErrorActionPreference = "Stop"
$compose = Join-Path $PSScriptRoot "docker-compose.yml"
$adapter = Join-Path $PSScriptRoot "oha-adapter.ps1"
. $adapter
$resultRoot = Join-Path $PSScriptRoot "results\$Version"
New-Item -ItemType Directory -Force -Path $resultRoot | Out-Null
$Cases = @($Cases | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
if ($ReferenceProfile) {
    # Match D:\Code\demo's A/B shape: one shared-iterations run, no extra
    # warm-up, the two 100-user endpoints, and the original VU sweep.
    $Cases = @("json-100-users", "users-static")
    $Runs = 1
    $WarmupSeconds = 0
}
$officialCases = @("plaintext", "static-json", "users-static", "static-route", "path-integer", "path-uuid", "validation-success", "query", "header", "json-small", "json-100-users", "postgres", "problem", "raw-handler", "security", "404", "405")
$officialVus = @(32, 64, 128, 256, 512)
$officialShapeValid =
    -not $Official -or (
        $Runs -ge 7 -and
        $Iterations -ge 1000000 -and
        (($Vus | Sort-Object) -join ',') -eq (($officialVus | Sort-Object) -join ',') -and
        (($Cases | Sort-Object) -join ',') -eq (($officialCases | Sort-Object) -join ',')
    )
if ($Official -and -not $officialShapeValid) {
    Write-Warning "Official release matrix is incomplete: require all official cases, VU 32/64/128/256/512, >=7 runs, and >=1,000,000 requests/run. Result will be INCONCLUSIVE."
}
$logicalProcessors = [Environment]::ProcessorCount
if ($logicalProcessors -lt 12 -and -not $AllowUndersizedHost) {
    throw "Official benchmark topology requires at least 12 logical processors; found $logicalProcessors. Use -AllowUndersizedHost only for a non-release smoke."
}

function Get-OhaCase([string]$case) {
    $spec = [ordered]@{
        Method = "GET"
        Path = "/plaintext"
        Headers = @()
        Body = $null
        ContentType = $null
        ExpectedStatus = 200
    }
    switch ($case) {
        "static-json" { $spec.Path = "/json-static" }
        "users-static" { $spec.Path = "/users-static" }
        "static-route" { $spec.Path = "/fixed/path" }
        "path-integer" { $spec.Path = "/users/123456" }
        "path-uuid" { $spec.Path = "/uuid/550e8400-e29b-41d4-a716-446655440000" }
        "validation-success" { $spec.Path = "/validation-success/42" }
        "query" { $spec.Path = "/search?page=42&active=true" }
        "header" {
            $spec.Path = "/trace"
            $spec.Headers = @("X-Trace-ID: abc123")
        }
        "json-small" { $spec.Path = "/json-small" }
        "json-100-users" { $spec.Path = "/users" }
        "postgres" { $spec.Path = "/users-db" }
        "problem" {
            $spec.Path = "/problem"
            $spec.ExpectedStatus = 400
        }
        "raw-handler" { $spec.Path = "/raw-handler" }
        "security" {
            $spec.Path = "/secure"
            $spec.Headers = @("X-API-Key: abc-secret")
        }
        "404" {
            $spec.Path = "/missing"
            $spec.ExpectedStatus = 404
        }
        "405" {
            $spec.Method = "POST"
            $spec.ExpectedStatus = 405
        }
        default { }
    }
    return [pscustomobject]$spec
}

function Get-OhaArguments($spec, [string]$targetUrl, [int]$connections, [int]$requestCount, [string]$outputPath, [switch]$SummaryOnly) {
    $outputFormat = if ($SummaryOnly) { "json" } else { "csv" }
    $arguments = @("-n", "$requestCount", "-c", "$connections", "--no-tui", "--output-format", $outputFormat, "--output", $outputPath)
    if ($spec.Method -ne "GET") { $arguments += @("--method", $spec.Method) }
    foreach ($header in $spec.Headers) { $arguments += @("-H", $header) }
    if ($null -ne $spec.Body) {
        $arguments += @("-d", $spec.Body)
        if ($null -ne $spec.ContentType) { $arguments += @("-T", $spec.ContentType) }
    }
    $arguments += "$targetUrl$($spec.Path)"
    return $arguments
}

function Assert-ContainerTopology([string]$service) {
    $container = (docker compose -f $compose ps -q $service).Trim()
    if (-not $container) { throw "No container found for $service" }
    $expectedCpu = switch ($service) {
        "postgres" { if ($env:POSTGRES_CPUSET) { $env:POSTGRES_CPUSET } else { "0-3" } }
        "raw" { if ($env:API_CPUSET) { $env:API_CPUSET } else { "0-3" } }
        "oas" { if ($env:API_CPUSET) { $env:API_CPUSET } else { "0-3" } }
        default { $null }
    }
    if ($expectedCpu) {
        $actualCpu = (docker inspect --format '{{.HostConfig.CpusetCpus}}' $container).Trim()
        if ($actualCpu -ne $expectedCpu) {
            throw "CPU affinity mismatch for ${service}: expected $expectedCpu, actual $actualCpu"
        }
    }
    $memorySetting = if ($service -eq "postgres") {
        if ($env:POSTGRES_MEMORY_LIMIT) { $env:POSTGRES_MEMORY_LIMIT } else { "512m" }
    } else {
        if ($env:API_MEMORY_LIMIT) { $env:API_MEMORY_LIMIT } else { "512m" }
    }
    $expectedMemory = switch -Regex ($memorySetting.ToLowerInvariant()) {
        '^([0-9]+)m$' { [int64]$matches[1] * 1MB; break }
        '^([0-9]+)g$' { [int64]$matches[1] * 1GB; break }
        default { throw "Unsupported memory limit: $memorySetting" }
    }
    $memoryLimit = [int64](docker inspect --format '{{.HostConfig.Memory}}' $container).Trim()
    if ($memoryLimit -ne $expectedMemory) {
        throw "Memory limit mismatch for ${service}: expected $memorySetting, actual $memoryLimit bytes"
    }
}

function Start-StatsCollector([string]$service, [string]$file) {
    $container = (docker compose -f $compose ps -q $service).Trim()
    if (-not $container) { return $null }
    "timestamp,cpu_percent,memory_usage,pids,cpu_usage_usec" | Set-Content -Path $file
    return Start-Job -ScriptBlock {
        param($containerId, $statsFile)
        while ($true) {
            $sample = docker stats --no-stream --format "{{.CPUPerc}},{{.MemUsage}},{{.PIDs}}" $containerId 2>$null
            $cpuUsageUsec = $null
            $cpuStat = @(docker exec $containerId cat /sys/fs/cgroup/cpu.stat 2>$null)
            foreach ($line in $cpuStat) {
                if ($line -match '^usage_usec\s+(\d+)$') {
                    $cpuUsageUsec = $matches[1]
                    break
                }
                if ($line -match '^\s*(\d+)\s*$') {
                    $cpuUsageUsec = [math]::Floor([double]$matches[1] / 1000)
                    break
                }
            }
            if ($sample) {
                "{0},{1},{2}" -f (Get-Date -Format o), $sample, $cpuUsageUsec | Add-Content -Path $statsFile
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

function Get-CgroupCpuUsageUsec([string]$service) {
    $container = (docker compose -f $compose ps -q $service).Trim()
    if (-not $container) { return $null }
    $cpuStat = @(docker exec $container cat /sys/fs/cgroup/cpu.stat 2>$null)
    foreach ($line in $cpuStat) {
        if ($line -match '^usage_usec\s+(\d+)$') {
            return [double]$matches[1]
        }
        if ($line -match '^\s*(\d+)\s*$') {
            return [math]::Floor([double]$matches[1] / 1000)
        }
    }
    return $null
}

function Get-NegativeTimingFields([string]$file) {
    if (-not (Test-Path $file)) { return @() }
    $summary = Get-Content -Raw -LiteralPath $file | ConvertFrom-Json
    $fields = @()
    foreach ($metricProperty in $summary.metrics.PSObject.Properties) {
        if ($metricProperty.Name -notlike 'http_req_*') { continue }
        $metric = $metricProperty.Value
        foreach ($field in @('min', 'med', 'max', 'p(95)', 'p(99)', 'p(99.9)')) {
            $property = $metric.PSObject.Properties[$field]
            if ($null -ne $property -and [double]$property.Value -lt 0) {
                $fields += "$($metricProperty.Name).$field"
            }
        }
    }
    return $fields
}

docker compose -f $compose build --no-cache
if ($LASTEXITCODE -ne 0) { throw "Docker image build failed" }

$records = [System.Collections.Generic.List[object]]::new()
$invalidTimingEvents = [System.Collections.Generic.List[string]]::new()
$stoppedEarly = $false
:BenchmarkMatrix foreach ($case in $Cases) {
:VusMatrix foreach ($vu in $Vus) {
    for ($run = 1; $run -le $Runs; $run++) {
        $order = @("raw", "oas") | Get-Random -Count 2
        foreach ($implementation in $order) {
            $service = $implementation
            $file = Join-Path $resultRoot "$case-$implementation-vu$vu-run$run.json"
            $outputName = if ($ReferenceProfile) { "$case-$implementation-vu$vu-run$run.oha.json" } else { "$case-$implementation-vu$vu-run$run.csv" }
            $outputFile = Join-Path $resultRoot $outputName
            $spec = Get-OhaCase $case
            $targetUrl = "http://{0}:8080" -f $service
            docker compose -f $compose down --remove-orphans | Out-Null
            if ($case -eq "postgres") {
                $env:DATABASE_URL = "postgres://bench:bench@postgres:5432/bench"
            } else {
                Remove-Item Env:DATABASE_URL -ErrorAction SilentlyContinue
            }
            $services = @($service)
            if ($case -eq "postgres") { $services = @("postgres", $service) }
            docker compose -f $compose up -d @services
            if ($LASTEXITCODE -ne 0) { throw "Failed to start $service" }
            Assert-ContainerTopology $service
            $postgresContainer = docker compose -f $compose ps -q postgres
            if ($postgresContainer) {
                Assert-ContainerTopology "postgres"
            }
            if ($WarmupSeconds -gt 0) {
                $warmupArgs = @("-z", "${WarmupSeconds}s", "-c", "$vu", "--no-tui", "--output-format", "quiet")
                if ($spec.Method -ne "GET") { $warmupArgs += @("--method", $spec.Method) }
                foreach ($header in $spec.Headers) { $warmupArgs += @("-H", $header) }
                if ($null -ne $spec.Body) {
                    $warmupArgs += @("-d", $spec.Body)
                    if ($null -ne $spec.ContentType) { $warmupArgs += @("-T", $spec.ContentType) }
                }
                $warmupArgs += "$targetUrl$($spec.Path)"
                docker compose -f $compose run --rm --no-deps oha @warmupArgs
                if ($LASTEXITCODE -ne 0) { throw "Warmup failed for $implementation vu=$vu run=$run" }
            }
            $statsFile = Join-Path $resultRoot "$case-$implementation-vu$vu-run$run.stats.csv"
            $statsJob = Start-StatsCollector $service $statsFile
            $cpuStartUsec = Get-CgroupCpuUsageUsec $service
            try {
                if (Test-Path $outputFile) { Remove-Item -LiteralPath $outputFile -Force }
                $ohaArgs = Get-OhaArguments $spec $targetUrl $vu $Iterations "/results/$outputName" -SummaryOnly:$ReferenceProfile
                docker compose -f $compose run --rm --no-deps oha @ohaArgs
                $exitCode = $LASTEXITCODE
            } finally {
                Stop-StatsCollector $statsJob
            }
            $cpuEndUsec = Get-CgroupCpuUsageUsec $service
            $sharedOutput = Join-Path $PSScriptRoot "results\$outputName"
            if (-not (Test-Path $sharedOutput)) { throw "oha did not write output for $implementation vu=$vu run=$run" }
            Move-Item -Force $sharedOutput $outputFile
            $summary = if ($ReferenceProfile) {
                Convert-OhaJsonToSummary -JsonPath $outputFile -ExpectedStatus $spec.ExpectedStatus -ExpectedRequests $Iterations
            } else {
                Convert-OhaCsvToSummary -CsvPath $outputFile -ExpectedStatus $spec.ExpectedStatus
            }
            $summary | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $file
            docker compose -f $compose down --remove-orphans | Out-Null
            if ($exitCode -ne 0) { throw "Benchmark failed for $implementation vu=$vu run=$run" }
            $records.Add([pscustomobject]@{
                case = $case
                implementation = $implementation
                vu = $vu
                run = $run
                file = $file
                output = $outputFile
                stats = $statsFile
                cpu_start_usec = $cpuStartUsec
                cpu_end_usec = $cpuEndUsec
            })
            $negativeTimingFields = Get-NegativeTimingFields $file
            if ($negativeTimingFields.Count -gt 0) {
                $event = "$case $implementation VU ${vu} run ${run}: $($negativeTimingFields -join ', ')"
                $invalidTimingEvents.Add($event)
                if (-not $ContinueAfterInvalidTiming) {
                    $stoppedEarly = $true
                    break BenchmarkMatrix
                }
            }
        }
    }
}
}

$expectedRecordCount = $Cases.Count * $Vus.Count * $Runs * 2
$matrixComplete = $records.Count -eq $expectedRecordCount
if ($Official -and -not $matrixComplete) {
    $officialShapeValid = $false
    Write-Warning "Official release matrix is incomplete: expected $expectedRecordCount measurements, collected $($records.Count). Result will be INCONCLUSIVE."
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

function Get-CpuNanosecondsPerRequest($record, $metric) {
    $requests = [double]$metric.metrics.measured_requests.count
    if ($requests -le 0) { return $null }
    if ($null -ne $record.cpu_start_usec -and $null -ne $record.cpu_end_usec) {
        $deltaUsec = [double]$record.cpu_end_usec - [double]$record.cpu_start_usec
        if ($deltaUsec -lt 0) { return $null }
        return $deltaUsec * 1000 / $requests
    }
    if (-not (Test-Path $record.stats)) { return $null }
    $values = @(Get-Content $record.stats | Select-Object -Skip 1 | ForEach-Object {
        $parts = $_ -split ',', 5
        if ($parts.Count -ge 5 -and $parts[4] -match '^\d+$') {
            [double]$parts[4]
        }
    })
    if ($values.Count -lt 2) { return $null }
    (($values | Measure-Object -Maximum).Maximum - ($values | Measure-Object -Minimum).Minimum) * 1000 / $requests
}

function Get-BootstrapMedianUpper95([double[]]$values) {
    if ($values.Count -lt 2) { return $null }
    $resamples = 10000
    $random = [System.Random]::new(8675309)
    $medians = [double[]]::new($resamples)
    for ($sampleIndex = 0; $sampleIndex -lt $resamples; $sampleIndex++) {
        $sample = [double[]]::new($values.Count)
        for ($valueIndex = 0; $valueIndex -lt $values.Count; $valueIndex++) {
            $sample[$valueIndex] = $values[$random.Next(0, $values.Count)]
        }
        $medians[$sampleIndex] = Get-Median $sample
    }
    $ordered = @($medians | Sort-Object)
    $index = [math]::Ceiling(0.95 * ($ordered.Count + 1)) - 1
    $index = [math]::Max(0, [math]::Min($ordered.Count - 1, $index))
    return [double]$ordered[$index]
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
    $ciUpper = Get-BootstrapMedianUpper95 $overheads
    $rawRpsValues = @($rawMetrics | ForEach-Object { [double]$_.metrics.measured_requests.rate })
    $rawMean = ($rawRpsValues | Measure-Object -Average).Average
    $rawCv = if ($rawMean -eq 0) { [double]::PositiveInfinity } else { (Get-StdDev $rawRpsValues) / $rawMean }
    $errors = @($rawMetrics + $oasMetrics | ForEach-Object { [double]$_.metrics.measured_errors.count } | Measure-Object -Sum).Sum
    $invalidRequestCount = @($rawMetrics + $oasMetrics | Where-Object {
        [double]$_.metrics.measured_requests.count -ne $Iterations
    }).Count -gt 0
    $invalidTiming = @($rawRecords + $oasRecords | ForEach-Object {
        (Get-NegativeTimingFields $_.file).Count -gt 0
    }) -contains $true
    $rawStatsRecords = @($rawRecords)
    $oasStatsRecords = @($oasRecords)
    $incompleteRuns = $rawMetrics.Count -lt $Runs -or $oasMetrics.Count -lt $Runs
    $rawCpuNsValues = @()
    $oasCpuNsValues = @()
    for ($index = 0; $index -lt [math]::Min($rawRecords.Count, $rawMetrics.Count); $index++) {
        $value = Get-CpuNanosecondsPerRequest $rawRecords[$index] $rawMetrics[$index]
        if ($null -ne $value) { $rawCpuNsValues += $value }
    }
    for ($index = 0; $index -lt [math]::Min($oasRecords.Count, $oasMetrics.Count); $index++) {
        $value = Get-CpuNanosecondsPerRequest $oasRecords[$index] $oasMetrics[$index]
        if ($null -ne $value) { $oasCpuNsValues += $value }
    }
    $rawCpuNs = Get-Median $rawCpuNsValues
    $oasCpuNs = Get-Median $oasCpuNsValues
    $rawMemory = Get-PeakMemoryMiB $rawStatsRecords
    $oasMemory = Get-PeakMemoryMiB $oasStatsRecords
    $cpuDelta = if ($null -eq $rawCpuNs -or $rawCpuNs -eq 0 -or $null -eq $oasCpuNs) { $null } else { ($oasCpuNs / $rawCpuNs) - 1 }
    $latencyFail = (($oasP50 / $rawP50) - 1) -gt 0.01 -or (($oasP95 / $rawP95) - 1) -gt 0.01 -or (($oasP99 / $rawP99) - 1) -gt 0.01
    $requiredEvidenceMissing = $null -eq $cpuDelta -or $null -eq $rawMemory -or $null -eq $oasMemory -or $null -eq $ciUpper
    $result = if (($Official -and -not $officialShapeValid) -or $errors -gt 0 -or $invalidTiming -or $invalidRequestCount -or $incompleteRuns -or $Runs -lt 7 -or $Iterations -lt 1000000 -or $rawCv -gt 0.005 -or $requiredEvidenceMissing) { "INCONCLUSIVE" } elseif ($medianOverhead -gt 0.01 -or $ciUpper -gt 0.01 -or $latencyFail -or $cpuDelta -gt 0.01) { "FAIL" } else { "PASS" }
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
        RawCpuNsPerRequest = $rawCpuNs
        OasCpuNsPerRequest = $oasCpuNs
        CpuDelta = $cpuDelta
        RawMemory = $rawMemory
        OasMemory = $oasMemory
        RawCv = $rawCv
        InvalidTiming = $invalidTiming
        InvalidRequestCount = $invalidRequestCount
        IncompleteRuns = $incompleteRuns
        CollectedRawRuns = $rawMetrics.Count
        CollectedOasRuns = $oasMetrics.Count
        MedianOverhead = $medianOverhead
        Upper95Ci = $ciUpper
        Result = $result
    }
}}
$table = if ($reportRows) {
    ($reportRows | ForEach-Object {
        $cpu = if ($null -eq $_.CpuDelta) { "n/a" } else { "$([math]::Round($_.CpuDelta * 100, 3))%" }
        $cpuNs = if ($null -eq $_.RawCpuNsPerRequest -or $null -eq $_.OasCpuNsPerRequest) { "n/a" } else { "$([math]::Round($_.RawCpuNsPerRequest, 2))/$([math]::Round($_.OasCpuNsPerRequest, 2))" }
        $rss = if ($null -eq $_.RawMemory -or $null -eq $_.OasMemory) { "n/a" } else { "$([math]::Round($_.RawMemory, 1))/$([math]::Round($_.OasMemory, 1))" }
        "| $($_.Case) (VU $($_.Vus)) | $([math]::Round($_.RawRps, 2)) | $([math]::Round($_.OasRps, 2)) | $([math]::Round($_.Overhead * 100, 3))% | $([math]::Round($_.RawP50, 4))/$([math]::Round($_.OasP50, 4)) | $([math]::Round($_.RawP95, 4))/$([math]::Round($_.OasP95, 4)) | $([math]::Round($_.RawP99, 4))/$([math]::Round($_.OasP99, 4)) | $([math]::Round($_.RawCv * 100, 3))% | $cpuNs | $cpu | $rss | $($_.Result) |"
    }) -join "`n"
} else {
    "| no completed cases | pending aggregation | pending aggregation | pending | pending | pending | pending | pending | pending | INCONCLUSIVE |"
}

$overall = if ($reportRows.Result -contains "FAIL" -and $officialShapeValid) { "FAIL" } elseif ($officialShapeValid -and $reportRows.Count -gt 0 -and @($reportRows.Result | Where-Object { $_ -ne "PASS" }).Count -eq 0) { "PASS" } else { "INCONCLUSIVE" }
$invalidRows = @($reportRows | Where-Object { $_.InvalidTiming } | ForEach-Object { "$($_.Case) VU $($_.Vus)" })
$timingTargets = @($invalidRows + $invalidTimingEvents)
$timingNote = if ($timingTargets.Count -gt 0) {
    "Timing invalidation: negative latency samples detected for $($timingTargets -join '; ')."
} else {
    "Timing invalidation: no negative latency samples detected in completed rows."
}
$eventNote = if ($invalidTimingEvents.Count -gt 0) {
    "Observed invalid timing fields: $($invalidTimingEvents -join '; ')."
} else {
    "Observed invalid timing fields: none."
}
$completionNote = if ($stoppedEarly) {
    "Execution stopped at the first invalid timing sample. Partial results are INCONCLUSIVE by construction."
} else {
    "Execution completed all requested case/VU/run loops."
}
$matrixNote = if ($Official -and -not $officialShapeValid) {
    "Official matrix guard: INCONCLUSIVE because the required case/VU/run/request tuple set was not complete."
} elseif ($Official) {
    "Official matrix guard: complete ($($records.Count) measurements)."
} else {
    "Official matrix guard: not requested; use -Official for release acceptance."
}

@"
# Benchmark report $Version

Status: $overall

This run collected $($records.Count) raw/framework measurements. Results are
classified only after the minimum run/request counts, raw-baseline CV, paired
95% bootstrap percentile bound for the paired median overhead, exact cgroup
CPU before/after samples, and RSS samples are available.

| Test | Raw RPS | OAS RPS | Overhead | p50 raw/oas (ms) | p95 raw/oas (ms) | p99 raw/oas (ms) | Raw CV | CPU ns/request raw/oas | CPU Δ | RSS peak raw/oas (MiB) | Result |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
${table}

$timingNote
$eventNote
$completionNote
$matrixNote

The statistical gate uses median paired throughput overhead, the upper 95%
percentile bootstrap CI from 10,000 resamples (seed 8675309), p50/p95/p99
latency deltas, raw baseline CV, zero measured errors, exact measured request
counts, cgroup CPU usage nanoseconds/request, memory samples, and the requested
run/request minimums. Any negative timing sample or incomplete request count
invalidates the row. p999 is retained in the JSON artifacts for warning analysis.
Authoritative CPU/request values use cgroup usage_usec captured immediately
before and after each measured oha run; docker stats remains charting evidence.
Allocation metrics are reported by the in-process router benchmark.

Raw result files and the exact environment must be retained beside this file.
"@ | Set-Content -Path (Join-Path $resultRoot "REPORT.md")

$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1 Name, NumberOfCores, NumberOfLogicalProcessors, MaxClockSpeed
$computer = Get-CimInstance Win32_ComputerSystem | Select-Object TotalPhysicalMemory, Manufacturer, Model
$os = Get-CimInstance Win32_OperatingSystem | Select-Object Caption, Version, BuildNumber
$gitSha = (git rev-parse HEAD).Trim()
$rustDetails = rustc -Vv | Out-String
$dockerDetails = docker version | Out-String
$composeVersion = docker compose version
$apiMemorySetting = if ($env:API_MEMORY_LIMIT) { $env:API_MEMORY_LIMIT } else { "512m" }
$postgresMemorySetting = if ($env:POSTGRES_MEMORY_LIMIT) { $env:POSTGRES_MEMORY_LIMIT } else { "512m" }
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
- PostgreSQL CPU affinity: $($env:POSTGRES_CPUSET) (default 0-3; matches the reference DB phase)
- Load-generator CPU affinity: $($env:LOAD_CPUSET) (default 4-7; matches the reference A/B benchmark)
- API/PostgreSQL memory limit: $apiMemorySetting / $postgresMemorySetting
- Host logical processors observed: $logicalProcessors
- Official topology guard: at least 12 logical processors unless -AllowUndersizedHost was explicitly used
- Rust compiler:

$rustDetails
- Docker Compose: $composeVersion
- Benchmark build image: rust:1.88.0-bookworm
- PostgreSQL image: postgres:16.4-bookworm
- oha image: ghcr.io/hatoo/oha:1.15.0 (digest pinned in docker-compose.yml)
- Release profile: opt-level=3, lto=fat, codegen-units=1, panic=abort, strip=true
- Dependencies are locked by Cargo.lock.

Docker version details are retained in docker-version.txt.
"@ | Set-Content (Join-Path $resultRoot "environment.md")
$dockerDetails | Set-Content (Join-Path $resultRoot "docker-version.txt")
$rustDetails | Set-Content (Join-Path $resultRoot "rust-version.txt")
@"
git_sha=$gitSha
profile=$(if ($ReferenceProfile) { "reference-compatible" } elseif ($Official) { "official" } else { "diagnostic" })
cases=$($Cases -join ',')
vus=$($Vus -join ',')
runs=$Runs
iterations=$Iterations
warmup_seconds=$WarmupSeconds
records=$($records.Count)
expected_records=$expectedRecordCount
official_shape_valid=$officialShapeValid
matrix_complete=$matrixComplete
"@ | Set-Content (Join-Path $resultRoot "manifest.txt")
Write-Host "Benchmark artifacts written to $resultRoot"
