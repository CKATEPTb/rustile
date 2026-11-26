#requires -Version 7.0
# Refresh the Cargo manifests and lockfiles after editing the root VERSION.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$version = (Get-Content -LiteralPath (Join-Path $root 'VERSION') -Raw).Trim()
$parsed = $null
if (-not [semver]::TryParse($version, [ref]$parsed) -or $version -cne $parsed.ToString()) {
    throw "VERSION must contain a full semantic version, got '$version'."
}

$field = [regex]'(?m)^version = "[^"\r\n]+"(?= # Generated from VERSION\.\r?$)'
foreach ($relativePath in 'Cargo.toml', 'packaging/Cargo.toml') {
    $manifest = Join-Path $root $relativePath
    $contents = [IO.File]::ReadAllText($manifest)
    if ($field.Matches($contents).Count -ne 1) {
        throw "Expected one generated package version in $relativePath."
    }
    $updated = $field.Replace($contents, ('version = "{0}"' -f $version))
    if ($updated -cne $contents) {
        [IO.File]::WriteAllText($manifest, $updated)
    }
    cargo update --workspace --manifest-path $manifest
    if ($LASTEXITCODE -ne 0) { throw "Could not refresh the lockfile for $relativePath." }
}
