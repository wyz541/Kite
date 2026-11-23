param(
    [switch]$Clean,
    [switch]$SkipKensaku
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSCommandPath
$dist = Join-Path $root 'dist'
$targetRoot = if ($env:CARGO_TARGET_DIR) {
    [IO.Path]::GetFullPath($env:CARGO_TARGET_DIR)
} else {
    Join-Path $root 'target'
}
$binary = Join-Path $targetRoot 'release\kite-explorer.exe'
$version = '0.6.89'
$releaseName = "Kite-v$version-windows-x64"
$staging = Join-Path $dist $releaseName
$archive = Join-Path $dist "$releaseName.zip"

Push-Location $root
try {
    if (-not $SkipKensaku -and ([string]::IsNullOrWhiteSpace($env:KENSAKU_CLI_PATH) -or [string]::IsNullOrWhiteSpace($env:KENSAKU_DAEMON_PATH))) {
        python scripts\kensaku\build_sidecars.py
        if ($LASTEXITCODE -ne 0) {
            throw 'Kensaku sidecar build failed'
        }
    }
}
finally {
    Pop-Location
}

Push-Location $root
try {
    if ($Clean) {
        cargo clean
    }

    cargo build --release --locked --features windows-ui
    New-Item -ItemType Directory -Force -Path $dist | Out-Null

    $resolvedDist = [IO.Path]::GetFullPath($dist).TrimEnd('\') + '\'
    $resolvedStaging = [IO.Path]::GetFullPath($staging)
    if (-not $resolvedStaging.StartsWith($resolvedDist, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Staging path is outside dist'
    }
    if (Test-Path -LiteralPath $resolvedStaging) {
        [IO.Directory]::Delete($resolvedStaging, $true)
    }

    New-Item -ItemType Directory -Force -Path $resolvedStaging | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $resolvedStaging 'icons') | Out-Null

    Copy-Item -LiteralPath $binary -Destination (Join-Path $resolvedStaging 'Kite.exe') -Force
    Copy-Item -LiteralPath (Join-Path $root 'README.md') -Destination $resolvedStaging -Force
    Copy-Item -LiteralPath (Join-Path $root 'LICENSE') -Destination $resolvedStaging -Force
    Copy-Item -LiteralPath (Join-Path $root 'assets\kite-icon-master.png') -Destination (Join-Path $resolvedStaging 'icons') -Force

    if (-not $SkipKensaku) {
        $kensakuCli = if ($env:KENSAKU_CLI_PATH) {
            $env:KENSAKU_CLI_PATH
        } else {
            Join-Path $root 'vendor\kensaku\kensaku-cli.exe'
        }
        $kensakuDaemon = if ($env:KENSAKU_DAEMON_PATH) {
            $env:KENSAKU_DAEMON_PATH
        } else {
            Join-Path $root 'vendor\kensaku\kensaku-daemon.exe'
        }
        foreach ($path in @($kensakuCli, $kensakuDaemon)) {
            if (-not (Test-Path -LiteralPath $path)) {
                throw "Kensaku sidecar not found: $path"
            }
        }
        Copy-Item -LiteralPath $kensakuCli -Destination (Join-Path $resolvedStaging 'kensaku-cli.exe') -Force
        Copy-Item -LiteralPath $kensakuDaemon -Destination (Join-Path $resolvedStaging 'kensaku-daemon.exe') -Force
        Copy-Item -LiteralPath (Join-Path $root 'docs\search\kensaku-backend.md') -Destination $resolvedStaging -Force
        Copy-Item -LiteralPath (Join-Path $root 'docs\search\KENSAKU_LICENSE') -Destination $resolvedStaging -Force
    }

    if (Test-Path -LiteralPath $archive) {
        [IO.File]::Delete($archive)
    }
    Compress-Archive -Path (Join-Path $resolvedStaging '*') -DestinationPath $archive -CompressionLevel Optimal

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zipArchive = [IO.Compression.ZipFile]::OpenRead($archive)
    try {
        $zipEntries = @($zipArchive.Entries.FullName)
    }
    finally {
        $zipArchive.Dispose()
    }
    $requiredEntries = @('Kite.exe')
    if (-not $SkipKensaku) {
        $requiredEntries += @('kensaku-cli.exe', 'kensaku-daemon.exe')
    }
    foreach ($requiredEntry in $requiredEntries) {
        if ($zipEntries -notcontains $requiredEntry) {
            throw "Release ZIP is missing $requiredEntry"
        }
    }

    $distBinary = Join-Path $dist 'Kite.exe'
    $binaryBytes = [IO.File]::ReadAllBytes($binary)
    $archiveBytes = [IO.File]::ReadAllBytes($archive)
    $eocdOffset = -1
    for ($offset = $archiveBytes.Length - 22; $offset -ge 0; $offset--) {
        if ([BitConverter]::ToUInt32($archiveBytes, $offset) -eq 0x06054b50) {
            $eocdOffset = $offset
            break
        }
    }
    if ($eocdOffset -lt 0) {
        throw 'Release ZIP has no end-of-central-directory record'
    }
    $entryCount = [BitConverter]::ToUInt16($archiveBytes, $eocdOffset + 10)
    $centralOffset = [BitConverter]::ToUInt32($archiveBytes, $eocdOffset + 16)
    $centralCursor = $centralOffset
    for ($index = 0; $index -lt $entryCount; $index++) {
        if ([BitConverter]::ToUInt32($archiveBytes, $centralCursor) -ne 0x02014b50) {
            throw "Release ZIP central directory is invalid at $centralCursor"
        }
        $localOffset = [BitConverter]::ToUInt32($archiveBytes, $centralCursor + 42)
        $adjustedOffset = [BitConverter]::GetBytes([uint32]($localOffset + $binaryBytes.Length))
        [Buffer]::BlockCopy($adjustedOffset, 0, $archiveBytes, $centralCursor + 42, 4)
        $nameLength = [BitConverter]::ToUInt16($archiveBytes, $centralCursor + 28)
        $extraLength = [BitConverter]::ToUInt16($archiveBytes, $centralCursor + 30)
        $commentLength = [BitConverter]::ToUInt16($archiveBytes, $centralCursor + 32)
        $centralCursor += 46 + $nameLength + $extraLength + $commentLength
    }
    $adjustedCentralOffset = [BitConverter]::GetBytes([uint32]($centralOffset + $binaryBytes.Length))
    [Buffer]::BlockCopy($adjustedCentralOffset, 0, $archiveBytes, $eocdOffset + 16, 4)
    $standaloneBytes = New-Object byte[] ($binaryBytes.Length + $archiveBytes.Length)
    [Buffer]::BlockCopy($binaryBytes, 0, $standaloneBytes, 0, $binaryBytes.Length)
    [Buffer]::BlockCopy($archiveBytes, 0, $standaloneBytes, $binaryBytes.Length, $archiveBytes.Length)
    [IO.File]::WriteAllBytes($distBinary, $standaloneBytes)

    $bootstrapValidation = Join-Path $targetRoot "kite-bootstrap-validation-$version"
    if (Test-Path -LiteralPath $bootstrapValidation) {
        [IO.Directory]::Delete($bootstrapValidation, $true)
    }
    try {
        Expand-Archive -LiteralPath $distBinary -DestinationPath $bootstrapValidation -Force
        foreach ($requiredEntry in $requiredEntries) {
            if (-not (Test-Path -LiteralPath (Join-Path $bootstrapValidation $requiredEntry))) {
                throw "Standalone update asset is missing $requiredEntry"
            }
        }
        $rawHash = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash
        $extractedHash = (Get-FileHash -LiteralPath (Join-Path $bootstrapValidation 'Kite.exe') -Algorithm SHA256).Hash
        if ($rawHash -ne $extractedHash) {
            throw 'Standalone update asset does not preserve the executable payload'
        }
    }
    finally {
        if (Test-Path -LiteralPath $bootstrapValidation) {
            [IO.Directory]::Delete($bootstrapValidation, $true)
        }
    }

    $exeHash = (Get-FileHash -LiteralPath $distBinary -Algorithm SHA256).Hash.ToLowerInvariant()
    $archiveHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath (Join-Path $dist 'SHA256SUMS.txt') -Encoding ascii -Value @(
        "$exeHash  Kite.exe"
        "$archiveHash  $releaseName.zip"
    )

    Write-Output $distBinary
    Write-Output $archive
}
finally {
    Pop-Location
}
