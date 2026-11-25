param([string]$Binary = (Join-Path $PSScriptRoot '../target/release/rustile.exe'))
$ErrorActionPreference = 'Stop'
$binaryPath = (Get-Item -LiteralPath $Binary).FullName
$manifest = Join-Path $PSScriptRoot '../Cargo.toml'
$artifacts = & cargo test --manifest-path $manifest --bin rustile --locked --no-run --message-format=json
if ($LASTEXITCODE -ne 0) { throw 'Unable to build the Manager geometry test.' }
$managerTests = ($artifacts | ForEach-Object { $_ | ConvertFrom-Json } |
    Where-Object { $_.reason -eq 'compiler-artifact' -and $_.profile.test -and $_.target.name -eq 'rustile' }).executable
if (-not $managerTests) { throw 'Missing Manager test binary.' }
$testDir = Join-Path ([IO.Path]::GetTempPath()) ('rustile-smoke-' + [Guid]::NewGuid())
$testExe = Join-Path $testDir 'desktop-smoke.exe'
$compiler = Join-Path $env:SystemRoot 'Microsoft.NET/Framework64/v4.0.30319/csc.exe'
$null = New-Item -ItemType Directory -Path $testDir
try {
    & $compiler /nologo /codepage:65001 /target:exe "/out:$testExe" /reference:System.Windows.Forms.dll /reference:System.Drawing.dll (Join-Path $PSScriptRoot 'desktop-smoke.cs')
    if ($LASTEXITCODE -ne 0) { throw 'Unable to build the isolated desktop fixture.' }
    & $testExe $binaryPath $managerTests
    if ($LASTEXITCODE -ne 0) { throw 'The isolated desktop smoke test failed.' }
} finally {
    $localAppData = Join-Path $testDir 'local-appdata'
    $settingsDir = Join-Path $localAppData 'Rustile'
    $settings = Join-Path $settingsDir 'settings.toml'
    if (Test-Path -LiteralPath $settings) { Remove-Item -LiteralPath $settings }
    if (Test-Path -LiteralPath $settingsDir) { Remove-Item -LiteralPath $settingsDir }
    if (Test-Path -LiteralPath $localAppData) { Remove-Item -LiteralPath $localAppData }
    if (Test-Path -LiteralPath $testExe) { Remove-Item -LiteralPath $testExe }
    Remove-Item -LiteralPath $testDir
}
