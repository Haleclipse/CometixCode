[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [string]$Version,
    [string]$InstallDir = (Join-Path $HOME '.local\bin')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repo = if ($env:NYAN_REPO) { $env:NYAN_REPO } else { 'blxzer77/nyan' }
if (-not $Version -and $env:NYAN_VERSION) {
    $Version = $env:NYAN_VERSION
}

if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne [System.Runtime.InteropServices.Architecture]::X64) {
    throw 'install.ps1 currently supports Windows x86_64 only.'
}

if (-not $Version) {
    $release = Invoke-RestMethod -Headers @{ Accept = 'application/vnd.github+json' } `
        -Uri "https://api.github.com/repos/$repo/releases/latest"
    $tag = $release.tag_name
} elseif ($Version.StartsWith('nyan-v')) {
    $tag = $Version
} else {
    $tag = "nyan-v$Version"
}

if ($tag -notmatch '^nyan-v[0-9]+\.[0-9]+\.[0-9]+(-(alpha|beta|rc)\.[0-9]+)?$') {
    throw "Invalid nyan release tag: $tag"
}

$asset = "nyan-$tag-windows-x86_64.zip"
$baseUrl = "https://github.com/$repo/releases/download/$tag"
$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("nyan-install-" + [guid]::NewGuid())
$archive = Join-Path $tempDir $asset
$sums = Join-Path $tempDir 'SHA256SUMS'
$unpacked = Join-Path $tempDir 'unpacked'

try {
    New-Item -ItemType Directory -Force $tempDir | Out-Null
    Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/$asset" -OutFile $archive
    Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/SHA256SUMS" -OutFile $sums

    $escapedAsset = [regex]::Escape($asset)
    $sumLine = Get-Content $sums | Where-Object { $_ -match "\s+$escapedAsset$" } | Select-Object -First 1
    if (-not $sumLine) {
        throw "SHA256SUMS has no entry for $asset"
    }
    $expected = ($sumLine -split '\s+')[0].ToLowerInvariant()
    if ($expected -notmatch '^[0-9a-f]{64}$') {
        throw "SHA256SUMS has an invalid digest for $asset"
    }
    $actual = (Get-FileHash -Algorithm SHA256 -Path $archive).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "SHA256 mismatch for $asset"
    }

    Expand-Archive -Path $archive -DestinationPath $unpacked
    New-Item -ItemType Directory -Force $InstallDir | Out-Null
    Copy-Item (Join-Path $unpacked 'nyan.exe') (Join-Path $InstallDir 'nyan.exe') -Force
    Write-Host "Installed nyan $($tag.Substring(5)) to $(Join-Path $InstallDir 'nyan.exe')"

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $pathEntries = if ($userPath) { $userPath -split ';' } else { @() }
    if ($pathEntries -notcontains $InstallDir) {
        Write-Host "Add the install directory to your user PATH with:"
        Write-Host "[Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path', 'User') + ';$InstallDir', 'User')"
        Write-Host 'Open a new PowerShell session after changing PATH.'
    }
} finally {
    if (Test-Path $tempDir) {
        Remove-Item -Recurse -Force $tempDir
    }
}
