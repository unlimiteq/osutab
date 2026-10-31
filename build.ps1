# Builds everything, in the order that lets the Windows program embed the rest: the tablet helper,
# the tablet app, the OpenTabletDriver bridge plugin, OpenTabletDriver itself, then the program.
#
# Needs: Rust, the .NET 10 SDK, and in tools\: jdk21, gradle and android-sdk for the tablet app.
# OpenTabletDriver's source is fetched into tools\otd-src at a pinned commit.
$ErrorActionPreference = 'Stop'
$tools = Join-Path $PSScriptRoot 'tools'
$otdCommit = 'f3927ac11146cb7dd5c3b43277d5d1e33e36f6d6'

cargo build --release -p osutab-helper --target aarch64-unknown-linux-musl
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

if (Test-Path (Join-Path $tools 'jdk21')) { $env:JAVA_HOME = Join-Path $tools 'jdk21' }
if (Test-Path (Join-Path $tools 'android-sdk')) { $env:ANDROID_HOME = Join-Path $tools 'android-sdk' }
$gradle = Join-Path $tools 'gradle\bin\gradle.bat'
if (-not (Test-Path $gradle)) { $gradle = 'gradle' }
& $gradle -p (Join-Path $PSScriptRoot 'android') --no-daemon assembleDebug testDebugUnitTest
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$otdSrc = Join-Path $tools 'otd-src'
if (-not (Test-Path $otdSrc)) {
  git clone https://github.com/OpenTabletDriver/OpenTabletDriver.git $otdSrc
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
  git -C $otdSrc checkout $otdCommit
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
dotnet build (Join-Path $PSScriptRoot 'otd-plugin\OsutabBridge.csproj') -c Release -v q --nologo
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$otdOut = Join-Path $PSScriptRoot 'target\otd'
foreach ($project in 'OpenTabletDriver.Daemon', 'OpenTabletDriver.UX.Wpf') {
  dotnet publish (Join-Path $otdSrc "$project\$project.csproj") -c Release -o $otdOut -v q --nologo
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

cargo build --release -p osutab-host
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
