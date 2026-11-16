# Builds everything, in the order that lets the Windows program embed the rest: the tablet helper,
# the tablet app, the OpenTabletDriver bridge plugin, OpenTabletDriver itself, then the program.
#
# Needs: Rust, the .NET 10 SDK, and in tools\: jdk21, gradle and android-sdk for the tablet app.
# OpenTabletDriver's source is fetched into tools\otd-src at a pinned commit.
$ErrorActionPreference = 'Stop'
# MSBuild's lingering helper processes, and the C# compiler's server, hold this script's output
# open long after it finishes.
$env:MSBUILDDISABLENODEREUSE = '1'
$env:UseSharedCompilation = 'false'
$tools = Join-Path $PSScriptRoot 'tools'
$otdCommit = 'f3927ac11146cb7dd5c3b43277d5d1e33e36f6d6'

# Compilers record where each source file was, which would put this PC's user folder in the
# programs. These flags write ~ and . instead. They go in CARGO_ENCODED_RUSTFLAGS because the
# folders can have spaces, and that replaces .cargo\config.toml's flags, so the helper repeats them.
$remap = @("--remap-path-prefix=$env:USERPROFILE=~", "--remap-path-prefix=$PSScriptRoot=.")
$sep = [string][char]0x1f
$helperFlags = @('-C', 'target-feature=+crt-static', '-C', 'link-self-contained=yes') + $remap
# .NET's builds map source folders the same way when they're marked as a CI build.
$dotnetPaths = @('-p:ContinuousIntegrationBuild=true', '-p:Deterministic=true')

$env:CARGO_ENCODED_RUSTFLAGS = $helperFlags -join $sep
cargo build --release -p osutab-helper --target aarch64-unknown-linux-musl
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# Gradle copies PATH into the test runner's command line, and a stray quote in PATH splits it.
$env:Path = $env:Path -replace '"', ''
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
dotnet build (Join-Path $PSScriptRoot 'otd-plugin\OsutabBridge.csproj') -c Release -v q --nologo @dotnetPaths
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$otdOut = Join-Path $PSScriptRoot 'target\otd'
foreach ($project in 'OpenTabletDriver.Daemon', 'OpenTabletDriver.UX.Wpf') {
  dotnet publish (Join-Path $otdSrc "$project\$project.csproj") -c Release -o $otdOut -v q --nologo @dotnetPaths
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

$env:CARGO_ENCODED_RUSTFLAGS = $remap -join $sep
cargo build --release -p osutab-host
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
