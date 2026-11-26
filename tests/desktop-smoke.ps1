[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$manifest = Join-Path $PSScriptRoot '../Cargo.toml'
# Build the desktop-restricted fixture separately from the production executable.
$fixtureTarget = Join-Path $PSScriptRoot '../target/installation-smoke'
$artifacts = & cargo build --manifest-path $manifest --bin rustile --release --locked --features installation-fixture --target-dir $fixtureTarget --message-format=json
if ($LASTEXITCODE -ne 0) { throw 'Unable to build the installation fixture.' }
$binaryPath = ($artifacts | ForEach-Object { $_ | ConvertFrom-Json } |
    Where-Object { $_.reason -eq 'compiler-artifact' -and $_.target.name -eq 'rustile' -and $_.executable }).executable
if (-not $binaryPath) { throw 'Missing installation fixture.' }
$artifacts = & cargo test --manifest-path $manifest --bin rustile --locked --no-run --message-format=json
if ($LASTEXITCODE -ne 0) { throw 'Unable to build the Manager geometry test.' }
$managerTests = ($artifacts | ForEach-Object { $_ | ConvertFrom-Json } |
    Where-Object { $_.reason -eq 'compiler-artifact' -and $_.profile.test -and $_.target.name -eq 'rustile' }).executable
if (-not $managerTests) { throw 'Missing Manager test binary.' }
$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')
$testName = 'rustile-smoke-' + [Guid]::NewGuid()
$testDir = Join-Path $tempRoot $testName
$testExe = Join-Path $testDir 'desktop-smoke.exe'
$appDir = Join-Path $testDir 'portable'
$portableExe = Join-Path $appDir 'rustile.exe'
$compiler = Join-Path $env:SystemRoot 'Microsoft.NET/Framework64/v4.0.30319/csc.exe'
$null = New-Item -ItemType Directory -Path $testDir
try {
    $null = New-Item -ItemType Directory -Path $appDir
    Copy-Item -LiteralPath $binaryPath -Destination $portableExe
    & $compiler /nologo /codepage:65001 /target:exe "/out:$testExe" /reference:System.Windows.Forms.dll /reference:System.Drawing.dll (Join-Path $PSScriptRoot 'desktop-smoke.cs')
    if ($LASTEXITCODE -ne 0) { throw 'Unable to build the isolated desktop fixture.' }
    & $testExe $portableExe $managerTests (Join-Path $PSScriptRoot '../locales')
    if ($LASTEXITCODE -ne 0) { throw 'The isolated desktop smoke test failed.' }
} finally {
    $resolved = Get-Item -LiteralPath $testDir
    if ($resolved.Parent.FullName -ne $tempRoot -or $resolved.Name -ne $testName -or
        ($resolved.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw 'Unexpected fixture directory during cleanup.'
    }
    Remove-Item -LiteralPath $resolved.FullName -Recurse -Force
}
