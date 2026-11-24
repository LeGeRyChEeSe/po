# Builds Po's Android app: the Rust core for the phone's processors, its Kotlin bindings, the APK.
#   pwsh -NoProfile -File scripts/build-android.ps1 [-Release] [-Test]
# Needs the Android SDK with an NDK (ANDROID_HOME, by default the Android Studio location of the
# platform), the Rust targets aarch64-linux-android and x86_64-linux-android, cargo-ndk, and a
# JDK 17+ (JAVA_HOME, by default Android Studio's own when it is found). Runs on Windows, Linux
# and macOS. -Test also runs the app's JVM unit tests.
param([switch]$Release, [switch]$Test)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

if (-not $env:ANDROID_HOME) {
    $env:ANDROID_HOME = if ($IsWindows) { Join-Path $env:LOCALAPPDATA 'Android/Sdk' }
        elseif ($IsMacOS) { Join-Path $HOME 'Library/Android/sdk' }
        else { Join-Path $HOME 'Android/Sdk' }
}
if (-not $env:ANDROID_NDK_HOME) {
    $env:ANDROID_NDK_HOME = (Get-ChildItem (Join-Path $env:ANDROID_HOME 'ndk') | Sort-Object Name | Select-Object -Last 1).FullName
}
if (-not $env:JAVA_HOME) {
    $studio = @(
        (Join-Path ($env:ProgramFiles ?? '') 'Android/Android Studio/jbr'),
        '/Applications/Android Studio.app/Contents/jbr/Contents/Home',
        '/opt/android-studio/jbr'
    ) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
    if ($studio) { $env:JAVA_HOME = $studio }
}

$app = Join-Path $root 'mobile/android'
Push-Location $root
try {
    # The core, for phones (arm64) and the emulator (x86_64).
    cargo ndk -t arm64-v8a -t x86_64 -P 26 -o (Join-Path $app 'app/src/main/jniLibs') build -j 2 -p po-mobile --release
    if ($LASTEXITCODE) { throw 'cargo ndk failed' }
    # The bindings come from a build that keeps UniFFI's metadata (the release one is stripped).
    cargo build -j 2 -p po-mobile
    if ($LASTEXITCODE) { throw 'host build failed' }
    $name = if ($IsWindows) { 'po_mobile.dll' } elseif ($IsMacOS) { 'libpo_mobile.dylib' } else { 'libpo_mobile.so' }
    $lib = Get-Item (Join-Path $root "target/debug/$name")
    cargo run -j 2 -q -p po-mobile --features bindgen --bin uniffi-bindgen -- generate --library $lib.FullName --language kotlin --out-dir (Join-Path $app 'app/src/main/java') --no-format
    if ($LASTEXITCODE) { throw 'bindings failed' }
    Set-Location $app
    Set-Content local.properties ("sdk.dir=" + $env:ANDROID_HOME.Replace([char]92, [char]47))
    $tasks = @(if ($Release) { 'assembleRelease' } else { 'assembleDebug' })
    if ($Test) { $tasks += 'testDebugUnitTest' }
    $gradle = if ($IsWindows) { '.\gradlew.bat' } else { './gradlew' }
    & $gradle @tasks --console=plain
    if ($LASTEXITCODE) { throw 'gradle failed' }
    Get-ChildItem -Recurse (Join-Path $app 'app/build/outputs/apk') -Filter *.apk | ForEach-Object { $_.FullName }
} finally {
    Pop-Location
}
