[CmdletBinding()]
param(
    [ValidateNotNullOrEmpty()][string]$InputExe,
    [ValidateNotNullOrEmpty()][string]$OutputExe = (Join-Path $PSScriptRoot '../target/dist/rustile.exe')
)

$ErrorActionPreference = 'Stop'
function Build-Executable([string]$Manifest, [string]$Name, [string]$TargetDir) {
    $cargoArgs = @('build', '--locked', '--release', '--manifest-path', $Manifest,
        '--bin', $Name, '--target', 'x86_64-pc-windows-msvc', '--message-format=json-render-diagnostics')
    if ($TargetDir) { $cargoArgs += @('--target-dir', $TargetDir) }
    $artifacts = & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { throw "Building $Name failed with exit code $LASTEXITCODE." }
    $binary = @($artifacts | ForEach-Object { $_ | ConvertFrom-Json } | Where-Object {
        $_.reason -eq 'compiler-artifact' -and $_.target.name -eq $Name -and $_.executable
    })
    if ($binary.Count -ne 1) { throw "Cargo did not report a unique $Name executable." }
    Get-Item -LiteralPath $binary[0].executable
}

$inputFile = if ($InputExe) {
    Get-Item -LiteralPath $InputExe
} else {
    Build-Executable (Join-Path $PSScriptRoot '../Cargo.toml') rustile
}
$destination = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputExe)
if ($inputFile.PSIsContainer -or $inputFile.Extension -ne '.exe' -or
    [IO.Path]::GetExtension($destination) -ne '.exe' -or $inputFile.FullName -eq $destination) {
    throw 'Specify separate input and output .exe files.'
}
$manifest = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../packaging/Cargo.toml')).ProviderPath
$tempRoot = (Get-Item -LiteralPath ([IO.Path]::GetTempPath())).FullName.TrimEnd('\')
$jobName = 'rustile-package-' + [guid]::NewGuid().ToString('N')
$work = Join-Path $tempRoot $jobName
$destinationDir = [IO.Path]::GetDirectoryName($destination)
$stage = Join-Path $destinationDir "$jobName.pending"
$previousCab = $env:RUSTILE_RELEASE_CAB
$previousHash = $env:RUSTILE_RELEASE_SHA256
$null = New-Item -ItemType Directory -Path $work
try {
    # A fixed local name gives makecab a stable archive member even when the
    # checkout path contains spaces or characters outside the active code page.
    Copy-Item -LiteralPath $inputFile.FullName -Destination (Join-Path $work 'rustile.exe')
    $cab = Join-Path $work 'payload.cab'
    Push-Location -LiteralPath $work
    $previousTmp = $env:TMP
    $previousTemp = $env:TEMP
    try {
        # makecab also creates intermediate files through legacy ANSI APIs.
        # A relative temp path works even if the Windows profile is Unicode.
        $env:TMP = '.'
        $env:TEMP = '.'
        & "$env:SystemRoot/System32/makecab.exe" /D CompressionType=LZX /D CompressionMemory=21 rustile.exe payload.cab | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "makecab failed with exit code $LASTEXITCODE." }

        # CFHEADER with no optional fields, one CFFOLDER and one CFFILE.
        # CFFOLDER.typeCompress is 0x1503 for LZX with a 2 MiB window.
        $bytes = [IO.File]::ReadAllBytes($cab)
        if ($bytes.Length -lt 72 -or [Text.Encoding]::ASCII.GetString($bytes, 0, 4) -cne 'MSCF' -or
            [BitConverter]::ToUInt16($bytes, 26) -ne 1 -or [BitConverter]::ToUInt16($bytes, 28) -ne 1 -or
            [BitConverter]::ToUInt16($bytes, 30) -ne 0 -or [BitConverter]::ToUInt32($bytes, 16) -ne 44 -or
            [BitConverter]::ToUInt16($bytes, 42) -ne 0x1503 -or
            [Text.Encoding]::ASCII.GetString($bytes, 60, 11) -cne 'rustile.exe' -or $bytes[71] -ne 0) {
            throw 'Expected one LZX-compressed rustile.exe in the release cabinet.'
        }
        & "$env:SystemRoot/System32/expand.exe" payload.cab verified.exe | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "expand failed with exit code $LASTEXITCODE." }
        $rawHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $inputFile.FullName).Hash
        if ((Get-FileHash -Algorithm SHA256 -LiteralPath 'verified.exe').Hash -ne $rawHash) {
            throw 'The expanded release payload differs from the input executable.'
        }
    } finally {
        $env:TMP = $previousTmp
        $env:TEMP = $previousTemp
        Pop-Location
    }

    $env:RUSTILE_RELEASE_CAB = $cab
    $env:RUSTILE_RELEASE_SHA256 = $rawHash
    # Separate target directories keep concurrent packagers from picking up
    # each other's embedded cabinet between linking and publishing the result.
    $package = Build-Executable $manifest rustile-package (Join-Path $work 'target')
    if ($package.Length -ge $inputFile.Length) { throw 'The packaged EXE must be smaller than the input EXE.' }

    $null = [IO.Directory]::CreateDirectory($destinationDir)
    [IO.File]::Copy($package.FullName, $stage)
    try {
        [IO.File]::Move($stage, $destination)
    } catch [IO.IOException] {
        if (-not [IO.File]::Exists($destination)) { throw }
        [IO.File]::Replace($stage, $destination, [System.Management.Automation.Language.NullString]::Value)
    }
    Write-Output ("Packaged {0:N0} bytes as {1:N0} bytes: {2}" -f $inputFile.Length, $package.Length, $destination)
} finally {
    $env:RUSTILE_RELEASE_CAB = $previousCab
    $env:RUSTILE_RELEASE_SHA256 = $previousHash
    if ([IO.File]::Exists($stage)) { Remove-Item -LiteralPath $stage }
    $created = Get-Item -LiteralPath $work
    if ($created.Parent.FullName -ne $tempRoot -or $created.Name -ne $jobName -or
        ($created.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw 'Unexpected packaging directory during cleanup.'
    }
    Remove-Item -LiteralPath $created.FullName -Recurse -Force
}
