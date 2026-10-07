[CmdletBinding()]
param(
    [string[]] $CrateNames = @('oas-rs', 'oas-rs-macros'),
    [string] $CrateDir = 'target/package',
    [string] $OutputDir = 'dist/cargo',
    [string] $RegistryIndex = 'sparse+https://storage.quangt.com/cargo/index/',
    [string] $DownloadBaseUrl = 'https://storage.quangt.com/cargo/crates'
)

$ErrorActionPreference = 'Stop'

function Write-Utf8NoBom {
    param(
        [Parameter(Mandatory = $true)] [string] $Path,
        [Parameter(Mandatory = $true)] [string] $Content
    )

    $parent = Split-Path -Parent $Path
    if ($parent) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }

    $utf8 = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::WriteAllText($Path, $Content, $utf8)
}

function Get-IndexRelativePath {
    param([Parameter(Mandatory = $true)] [string] $Name)

    $lower = $Name.ToLowerInvariant()
    if ($lower.Length -eq 1) {
        return Join-Path '1' $lower
    }
    if ($lower.Length -eq 2) {
        return Join-Path '2' $lower
    }
    if ($lower.Length -eq 3) {
        return Join-Path (Join-Path '3' $lower.Substring(0, 1)) $lower
    }

    return Join-Path (Join-Path $lower.Substring(0, 2) $lower.Substring(2, 2)) $lower
}

function Convert-DependencyKind {
    param($Kind)

    if ([string]::IsNullOrEmpty([string] $Kind)) {
        return 'normal'
    }
    return [string] $Kind
}

$metadata = cargo metadata --format-version 1 --locked --no-deps | ConvertFrom-Json
$packagesByName = @{}
foreach ($package in $metadata.packages) {
    $packagesByName[$package.name] = $package
}
$requestedCrates = @($CrateNames | ForEach-Object { $_ -split ',' } | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })

$indexRoot = Join-Path $OutputDir 'index'
$crateOutputRoot = Join-Path $OutputDir 'crates'
$config = [ordered]@{
    dl = "$($DownloadBaseUrl.TrimEnd('/'))/{crate}/{version}/{crate}-{version}.crate"
}
Write-Utf8NoBom -Path (Join-Path $indexRoot 'config.json') -Content (($config | ConvertTo-Json -Compress -Depth 10) + "`n")

$cratesIoIndex = 'https://github.com/rust-lang/crates.io-index'
$normalizedRegistryIndex = $RegistryIndex.TrimEnd('/')

foreach ($crateName in $requestedCrates) {
    if (-not $packagesByName.ContainsKey($crateName)) {
        throw "Package '$crateName' was not found in cargo metadata."
    }

    $package = $packagesByName[$crateName]
    $crateFile = Join-Path $CrateDir "$($package.name)-$($package.version).crate"
    if (-not (Test-Path -LiteralPath $crateFile -PathType Leaf)) {
        throw "Missing crate artifact: $crateFile"
    }

    $dependencyEntries = @()
    foreach ($dependency in $package.dependencies) {
        $dependencyRegistry = $null
        if (-not [string]::IsNullOrEmpty([string] $dependency.registry)) {
            if ($dependency.registry.TrimEnd('/') -ne $normalizedRegistryIndex) {
                $dependencyRegistry = $dependency.registry.TrimEnd('/')
            }
        } elseif (-not [string]::IsNullOrEmpty([string] $dependency.source)) {
            $dependencyRegistry = $cratesIoIndex
        }

        $dependencyEntry = [ordered]@{
            name = if ([string]::IsNullOrEmpty([string] $dependency.rename)) { $dependency.name } else { $dependency.rename }
            req = $dependency.req
            features = @($dependency.features)
            optional = [bool] $dependency.optional
            default_features = [bool] $dependency.uses_default_features
            target = $dependency.target
            kind = Convert-DependencyKind $dependency.kind
            registry = $dependencyRegistry
        }

        if (-not [string]::IsNullOrEmpty([string] $dependency.rename)) {
            $dependencyEntry.package = $dependency.name
        }

        $dependencyEntries += $dependencyEntry
    }

    $indexEntry = [ordered]@{
        name = $package.name
        vers = $package.version
        deps = $dependencyEntries
        cksum = (Get-FileHash -Algorithm SHA256 -LiteralPath $crateFile).Hash.ToLowerInvariant()
        features = $package.features
        yanked = $false
        links = $package.links
        v = 2
    }

    if (-not [string]::IsNullOrEmpty([string] $package.rust_version)) {
        $indexEntry.rust_version = $package.rust_version
    }

    $relativeIndexPath = Get-IndexRelativePath -Name $package.name
    $indexFile = Join-Path $indexRoot $relativeIndexPath
    $entryJson = $indexEntry | ConvertTo-Json -Compress -Depth 100

    $existingLines = @()
    if (Test-Path -LiteralPath $indexFile -PathType Leaf) {
        $existingLines = @(Get-Content -LiteralPath $indexFile)
    }

    $sameVersion = $existingLines | Where-Object {
        try {
            ((ConvertFrom-Json $_).vers -eq $package.version)
        } catch {
            throw "Invalid JSON in existing index file: $indexFile"
        }
    }

    if ($sameVersion) {
        $existingEntry = $existingLines | Where-Object { (ConvertFrom-Json $_).vers -eq $package.version } | Select-Object -First 1
        if ($existingEntry -ne $entryJson) {
            throw "Version $($package.name) $($package.version) already exists with different metadata in $indexFile"
        }
    } else {
        $newContent = @($existingLines + $entryJson) -join "`n"
        Write-Utf8NoBom -Path $indexFile -Content ($newContent + "`n")
    }

    $crateDestination = Join-Path (Join-Path $crateOutputRoot $package.name) $package.version
    New-Item -ItemType Directory -Path $crateDestination -Force | Out-Null
    Copy-Item -LiteralPath $crateFile -Destination (Join-Path $crateDestination (Split-Path -Leaf $crateFile)) -Force

    Write-Output "Prepared $($package.name) $($package.version)"
}
