<#
.SYNOPSIS
Install the websift binary from a published GitHub release.

.EXAMPLE
irm https://raw.githubusercontent.com/suiflex/websift/main/install.ps1 | iex

.NOTES
Environment overrides:
  WEBSIFT_VERSION      release tag to install (default: latest)
  WEBSIFT_INSTALL_DIR  destination directory (default: %LOCALAPPDATA%\Programs\websift)
#>

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = 'suiflex/websift'
$installDir = if ($env:WEBSIFT_INSTALL_DIR) {
    $env:WEBSIFT_INSTALL_DIR
} else {
    Join-Path $env:LOCALAPPDATA 'Programs\websift'
}

$arch = switch ($env:PROCESSOR_ARCHITECTURE) {
    'AMD64' { 'x86_64' }
    'ARM64' { 'aarch64' }
    default { throw "install: unsupported architecture: $($env:PROCESSOR_ARCHITECTURE)" }
}
$target = "$arch-pc-windows-msvc"

$version = $env:WEBSIFT_VERSION
if (-not $version) {
    $latest = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest" -Headers @{ 'User-Agent' = 'websift-installer' }
    $version = $latest.tag_name
    if (-not $version) { throw 'install: could not determine the latest release; set WEBSIFT_VERSION and retry' }
}

$asset = "websift-$version-$target.zip"
$base = "https://github.com/$repo/releases/download/$version"
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $tmp | Out-Null

try {
    Write-Host "install: downloading $asset"
    try {
        Invoke-WebRequest -Uri "$base/$asset" -OutFile (Join-Path $tmp $asset)
        Invoke-WebRequest -Uri "$base/$asset.sha256" -OutFile (Join-Path $tmp "$asset.sha256")
    } catch {
        throw "install: no verified release asset for $target at $version; build from source with: cargo install --git https://github.com/$repo"
    }

    # Verify before extracting: a tampered archive must never reach the filesystem.
    $expected = ((Get-Content (Join-Path $tmp "$asset.sha256") -Raw) -split '\s+')[0]
    $actual = (Get-FileHash -Path (Join-Path $tmp $asset) -Algorithm SHA256).Hash
    if (-not $expected) { throw 'install: checksum file was empty' }
    if ($expected -ne $actual) { throw "install: checksum mismatch for $asset; expected $expected, got $actual" }

    Expand-Archive -Path (Join-Path $tmp $asset) -DestinationPath $tmp -Force
    $binary = Join-Path $tmp 'websift.exe'
    if (-not (Test-Path $binary)) { throw 'install: release archive did not contain websift.exe' }

    New-Item -ItemType Directory -Path $installDir -Force | Out-Null
    Move-Item -Path $binary -Destination (Join-Path $installDir 'websift.exe') -Force
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

Write-Host "install: websift $version installed to $installDir\websift.exe"

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$installDir*") {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$installDir", 'User')
    Write-Host "install: added $installDir to your user PATH; reopen your terminal"
}

Write-Host @"

Register the server with an agent (nothing else to configure; search works out of the box):

  claude mcp add --scope user websift -- $installDir\websift.exe mcp --profile claude-code
  codex mcp add websift -- $installDir\websift.exe mcp --profile codex

Check the installation:

  $installDir\websift.exe doctor
"@
