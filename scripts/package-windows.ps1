$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$previousRustFlags = $env:RUSTFLAGS
Push-Location (Join-Path $PSScriptRoot '..')
try {
    $root = (Get-Location).Path
    $target = 'x86_64-pc-windows-msvc'
    $env:RUSTFLAGS = "$previousRustFlags -C target-feature=+crt-static".Trim()
    & cargo build --release --locked --target $target
    if ($LASTEXITCODE -ne 0) { throw 'Windows release build failed.' }
    $metadata = & cargo metadata --no-deps --locked --format-version 1
    if ($LASTEXITCODE -ne 0) { throw 'Could not read the package version.' }
    $version = ($metadata | ConvertFrom-Json).packages[0].version
    $numericVersion = ($version -split '[-+]')[0]
    $binary = Join-Path $root "target\$target\release\unidirstat.exe"
    $output = Join-Path $root 'target\packages'
    New-Item -ItemType Directory -Force $output | Out-Null
    $compiler = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($null -eq $compiler) {
        $compiler = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'
    } else {
        $compiler = $compiler.Source
    }
    if (-not (Test-Path $compiler)) { throw 'Install Inno Setup 6 or add ISCC.exe to PATH.' }
    & $compiler "/DAppVersion=$version" "/DNumericVersion=$numericVersion" "/DBinaryPath=$binary" "/DProjectRoot=$root" "/DOutputPath=$output" 'packaging\windows\unidirstat.iss'
    if ($LASTEXITCODE -ne 0) { throw 'Windows installer compilation failed.' }
    $installer = Join-Path $output "UniDirStat-$version-windows-x86_64-setup.exe"
    if (-not (Test-Path $installer)) { throw 'Windows installer is missing.' }
    & $binary --help | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'The packaged executable did not start.' }
    Write-Output $installer
} finally {
    $env:RUSTFLAGS = $previousRustFlags
    Pop-Location
}
