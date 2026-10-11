[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot

Push-Location $projectRoot
try {
    & dotnet tool restore --add-source https://api.nuget.org/v3/index.json
    if ($LASTEXITCODE -ne 0) { throw 'Could not restore the pinned WiX tool.' }

    $metadataJson = & cargo metadata --no-deps --format-version 1 --locked
    if ($LASTEXITCODE -ne 0) { throw 'Could not read Cargo package metadata.' }
    $package = ($metadataJson | ConvertFrom-Json).packages | Where-Object { $_.name -eq 'disk-light' }
    if (-not $package) { throw 'Cargo metadata did not contain disk-light.' }
    if ($package.version -notmatch '^\d+\.\d+\.\d+$') {
        throw 'MSI releases require a numeric major.minor.patch Cargo version.'
    }

    & cargo build --release --locked --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }

    $output = Join-Path $projectRoot "dist\DiskLight-$($package.version)-windows-x64.msi"
    New-Item -ItemType Directory -Force (Split-Path $output) | Out-Null
    & dotnet tool run wix -- build installer/disk-light.wxs -arch x64 `
        -d "AppVersion=$($package.version)" `
        -d "BinaryPath=$projectRoot\target\x86_64-pc-windows-msvc\release\disk-light.exe" `
        -d "IconPath=$projectRoot\assets\disk-light.ico" `
        -intermediatefolder target/wix -o $output
    if ($LASTEXITCODE -ne 0) { throw 'MSI compilation failed.' }

    & dotnet tool run wix -- msi validate $output
    if ($LASTEXITCODE -ne 0) { throw 'MSI validation failed.' }
    Write-Host "Installer: $output"
} finally {
    Pop-Location
}
