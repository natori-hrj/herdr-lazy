# Produce target/release/herdr-lazy.exe, preferring a checksum-verified Windows release asset.
#
# Herdr runs Windows build commands without assuming a POSIX shell. A failed or unverifiable
# download always falls back to Cargo, so adding this fast path cannot turn an install that
# used to work into one that trusts an unverified binary.

$ErrorActionPreference = "Stop"

$repo = "natori-hrj/herdr-lazy"
$root = Split-Path -Parent $PSScriptRoot
$outDir = Join-Path $root "target\release"
$out = Join-Path $outDir "herdr-lazy.exe"
$target = "x86_64-pc-windows-msvc"

function Write-Log([string]$Message) {
    Write-Error "[herdr-lazy build] $Message" -ErrorAction Continue
}

function Build-FromSource {
    $cargo = Get-Command cargo -ErrorAction SilentlyContinue
    if ($null -eq $cargo) {
        throw "no prebuilt binary was usable, and cargo could not be found"
    }

    Write-Log "building from source with $($cargo.Source)"
    & $cargo.Source build --release
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed with exit code $LASTEXITCODE"
    }
}

function Fallback([string]$Reason) {
    if ($Reason) {
        Write-Log $Reason
    }
    try {
        Build-FromSource
        exit 0
    }
    catch {
        Write-Log $_.Exception.Message
        exit 1
    }
}

Set-Location -LiteralPath $root
$manifest = Join-Path $root "herdr-plugin.toml"
$versionMatch = Select-String -LiteralPath $manifest -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
if ($null -eq $versionMatch) {
    Fallback "could not read the plugin version"
}
$version = $versionMatch.Matches[0].Groups[1].Value

$fingerprintScript = Join-Path $PSScriptRoot "build-fingerprint.ps1"
try {
    $fingerprint = ((& $fingerprintScript) -join "").Trim()
    if ($fingerprint -notmatch '^[0-9a-f]{12}$') {
        throw "fingerprint script returned an invalid value"
    }
}
catch {
    Fallback "could not fingerprint the source: $($_.Exception.Message)"
}

$asset = "herdr-lazy-$version-$fingerprint-$target.tar.gz"
$base = $env:HERDR_LAZY_RELEASE_BASE
if ([string]::IsNullOrWhiteSpace($base)) {
    $base = "https://github.com/$repo/releases/download/v$version"
}
$base = $base.TrimEnd('/')

$temp = Join-Path ([System.IO.Path]::GetTempPath()) ("herdr-lazy-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $temp | Out-Null

try {
    $archive = Join-Path $temp $asset
    $checksum = "${archive}.sha256"
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile $archive
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset.sha256" -OutFile $checksum

    $want = ((Get-Content -LiteralPath $checksum -Raw).Trim() -split '\s+')[0].ToLowerInvariant()
    $got = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($want -ne $got) {
        throw "checksum mismatch (expected $want, got $got)"
    }

    $extract = Join-Path $temp "extract"
    New-Item -ItemType Directory -Path $extract | Out-Null
    & tar -xzf $archive -C $extract
    if ($LASTEXITCODE -ne 0) {
        throw "could not extract $asset"
    }

    $downloaded = Join-Path $extract "herdr-lazy.exe"
    if (-not (Test-Path -LiteralPath $downloaded -PathType Leaf)) {
        throw "archive did not contain herdr-lazy.exe"
    }

    New-Item -ItemType Directory -Path $outDir -Force | Out-Null
    $staged = "${out}.new"
    Copy-Item -LiteralPath $downloaded -Destination $staged -Force
    Move-Item -LiteralPath $staged -Destination $out -Force
    Write-Log "installed prebuilt binary (sha256 $got)"
    exit 0
}
catch {
    Fallback "prebuilt binary unavailable: $($_.Exception.Message)"
}
finally {
    Remove-Item -LiteralPath $temp -Recurse -Force -ErrorAction SilentlyContinue
}
