param(
    [string]$Destination = (Join-Path $PSScriptRoot '..\resources\ffmpeg\runtime')
)

$ErrorActionPreference = 'Stop'

$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$destinationPath = [System.IO.Path]::GetFullPath($Destination)
$workspacePrefix = $workspace.TrimEnd([System.IO.Path]::DirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar

if (-not $destinationPath.StartsWith($workspacePrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Destination must remain inside the Free-Train workspace: $destinationPath"
}

$downloadUrl = 'https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-lgpl-shared.zip'
$tempRoot = Join-Path $env:TEMP ('freetrain-ffmpeg-' + [guid]::NewGuid().ToString('N'))
$archivePath = Join-Path $tempRoot 'ffmpeg-lgpl-shared.zip'
$extractPath = Join-Path $tempRoot 'extract'
$stagePath = Join-Path $tempRoot 'runtime'

New-Item -ItemType Directory -Force -Path $tempRoot, $extractPath, $stagePath | Out-Null

try {
    Write-Host 'Downloading FFmpeg LGPL shared build...'
    Invoke-WebRequest -Uri $downloadUrl -OutFile $archivePath
    Expand-Archive -LiteralPath $archivePath -DestinationPath $extractPath

    $distribution = Get-ChildItem -LiteralPath $extractPath -Directory | Select-Object -First 1
    if (-not $distribution) {
        throw 'Downloaded archive does not contain an FFmpeg distribution directory.'
    }

    $binPath = Join-Path $distribution.FullName 'bin'
    $runtimeFiles = Get-ChildItem -LiteralPath $binPath -File | Where-Object {
        $_.Extension -eq '.dll' -or $_.Name -in @('ffmpeg.exe', 'ffprobe.exe')
    }
    $runtimeFiles | Copy-Item -Destination $stagePath
    Copy-Item -LiteralPath (Join-Path $distribution.FullName 'LICENSE.txt') -Destination $stagePath

    $ffmpegPath = Join-Path $stagePath 'ffmpeg.exe'
    $versionOutput = & $ffmpegPath -version 2>&1 | Out-String
    if ($versionOutput -match '--enable-gpl') {
        throw 'The downloaded FFmpeg build unexpectedly enables GPL mode.'
    }
    if ($versionOutput -notmatch '--disable-libx264' -or $versionOutput -notmatch '--disable-libx265') {
        throw 'The downloaded FFmpeg build does not match the expected LGPL codec configuration.'
    }

    if (Test-Path -LiteralPath $destinationPath) {
        $resolvedDestination = [System.IO.Path]::GetFullPath($destinationPath)
        if (-not $resolvedDestination.StartsWith($workspacePrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing to replace destination outside workspace: $resolvedDestination"
        }
        Remove-Item -LiteralPath $resolvedDestination -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $destinationPath | Out-Null
    Copy-Item -Path (Join-Path $stagePath '*') -Destination $destinationPath

    $bytes = (Get-ChildItem -LiteralPath $destinationPath -File | Measure-Object Length -Sum).Sum
    Write-Host ("FFmpeg runtime staged at {0} ({1:N2} MiB)." -f $destinationPath, ($bytes / 1MB))
}
finally {
    if (Test-Path -LiteralPath $tempRoot) {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force
    }
}

