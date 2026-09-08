# Print the same source fingerprint as build-fingerprint.sh.
#
# The Windows install path cannot assume a POSIX shell, so keep this implementation beside
# the Unix one. The CI release job compares both outputs on Windows; a mismatch makes the
# installer fall back to a local build instead of ever accepting a binary for different source.

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$files = New-Object 'System.Collections.Generic.List[string]'
$sourceFiles = New-Object 'System.Collections.Generic.List[string]'

foreach ($relative in @("Cargo.toml", "Cargo.lock")) {
    $path = Join-Path $root ($relative -replace '/', '\')
    if (Test-Path -LiteralPath $path -PathType Leaf) {
        [void]$files.Add($relative)
    }
}

$src = Join-Path $root "src"
if (Test-Path -LiteralPath $src -PathType Container) {
    Get-ChildItem -LiteralPath $src -Recurse -File -Filter "*.rs" |
        ForEach-Object {
            $relative = $_.FullName.Substring($root.Length + 1).Replace('\', '/')
            [void]$sourceFiles.Add($relative)
        }
}

# build-fingerprint.sh keeps the manifest files in a fixed order and uses LC_ALL=C sort only
# for source paths. Ordinal comparison is the PowerShell equivalent for these repository-
# relative paths and does not vary with the user's locale.
$sourceFiles.Sort([System.StringComparer]::Ordinal)
foreach ($relative in $sourceFiles) {
    [void]$files.Add($relative)
}

$stream = New-Object System.IO.MemoryStream
try {
    foreach ($relative in $files) {
        $name = [System.Text.Encoding]::UTF8.GetBytes("$relative`n")
        $stream.Write($name, 0, $name.Length)

        $path = Join-Path $root ($relative -replace '/', '\')
        $content = [System.IO.File]::ReadAllBytes($path)
        $stream.Write($content, 0, $content.Length)
    }

    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $digest = $sha.ComputeHash($stream.ToArray())
    }
    finally {
        $sha.Dispose()
    }
}
finally {
    $stream.Dispose()
}

[BitConverter]::ToString($digest).Replace('-', '').ToLowerInvariant().Substring(0, 12)
