# Builds Po's Android app: the Rust core for the phone's processors, its Kotlin bindings, the APK.
#   pwsh -NoProfile -File scripts/build-android.ps1 [-Release]
# Needs the Android SDK with an NDK (ANDROID_HOME, default %LOCALAPPDATA%\Android\Sdk), the Rust
# targets aarch64-linux-android and x86_64-linux-android, cargo-ndk, and a JDK 17+ (JAVA_HOME,
# default Android Studio's).
param([switch]$Release)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $env:ANDROID_HOME) { $env:ANDROID_HOME = "$env:LOCALAPPDATA\Android\Sdk" }
if (-not $env:ANDROID_NDK_HOME) {
    $env:ANDROID_NDK_HOME = (Get-ChildItem "$env:ANDROID_HOME\ndk" | Sort-Object Name | Select-Object -Last 1).FullName
}
if (-not $env:JAVA_HOME) { $env:JAVA_HOME = 'E:\Applications\Android\Android Studio\jbr' }
$app = Join-Path $root 'mobile\android'
Push-Location $root
try {
    # The core, for phones (arm64) and the emulator (x86_64).
    cargo ndk -t arm64-v8a -t x86_64 -P 26 -o "$app\app\src\main\jniLibs" build -j 2 -p po-mobile --release
    if ($LASTEXITCODE) { throw 'cargo ndk failed' }
    # The bindings come from a build that keeps UniFFI's metadata (the release one is stripped).
    cargo build -j 2 -p po-mobile
    $lib = Get-Item (Join-Path $root "target/debug/po_mobile.dll")
    cargo run -j 2 -q -p po-mobile --features bindgen --bin uniffi-bindgen -- generate --library $lib.FullName --language kotlin --out-dir "$app\app\src\main\java" --no-format
    if ($LASTEXITCODE) { throw 'bindings failed' }
    Set-Location $app
    Set-Content local.properties ("sdk.dir=" + $env:ANDROID_HOME.Replace([char]92, [char]47))
    $task = if ($Release) { 'assembleRelease' } else { 'assembleDebug' }
    .\gradlew.bat $task --console=plain
    if ($LASTEXITCODE) { throw 'gradle failed' }
    Get-ChildItem -Recurse "$app\app\build\outputs\apk" -Filter *.apk | ForEach-Object { $_.FullName }
} finally {
    Pop-Location
}
