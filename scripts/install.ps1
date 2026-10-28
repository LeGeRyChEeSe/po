# Builds Po in release mode and installs it into Cargo's binary folder (already on the
# PATH): `po` can then be launched from any folder, in any terminal.
# Run again after every change to Po; the build is incremental.
#
#   pwsh -File scripts/install.ps1

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

cargo build --release -p po --manifest-path (Join-Path $root 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$bin = Join-Path $cargoHome 'bin'
New-Item -ItemType Directory -Force $bin | Out-Null
$target = Join-Path $bin 'po.exe'

# Windows refuses to overwrite a running executable but allows renaming it: an open Po
# session keeps running the old version, new ones pick up the new one.
# Each old copy gets a unique name (a session may still hold the previous one);
# copies that no session uses anymore are deleted along the way.
Get-ChildItem $bin -Filter 'po.old*.exe' | ForEach-Object {
    try { Remove-Item $_.FullName -Force -ErrorAction Stop } catch {}
}
if (Test-Path $target) {
    $old = Join-Path $bin ("po.old-{0}.exe" -f (Get-Date -Format 'yyyyMMdd-HHmmss'))
    Move-Item $target $old -Force
}
Copy-Item (Join-Path $root 'target\release\po.exe') $target
Get-ChildItem $bin -Filter 'po.old*.exe' | ForEach-Object {
    try { Remove-Item $_.FullName -Force -ErrorAction Stop } catch {}
}

Write-Host "installed: $target"
& $target --version
