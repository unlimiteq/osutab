# Builds everything and puts a ready-to-run copy in dist\osu!tab. That folder can be moved anywhere.
# With -Zip it also makes the release download: a zip of that folder, and its SHA-256.
#
# Nothing from Google is included. Direct USB, the default, talks to the tablet itself. The adb.exe
# USB path still works if you put Google's platform-tools folder beside osu!tab.exe yourself.
param([switch]$Zip)
$ErrorActionPreference = 'Stop'
& (Join-Path $PSScriptRoot 'build.ps1')
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$version = (Select-String -Path (Join-Path $PSScriptRoot 'crates\host\Cargo.toml') -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$dist = Join-Path $PSScriptRoot 'dist\osu!tab'
Remove-Item -Recurse -Force $dist -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $dist | Out-Null
# Cargo can't name a program with a "!", so the exe gets its real name here.
Copy-Item (Join-Path $PSScriptRoot 'target\release\osutab.exe') (Join-Path $dist 'osu!tab.exe') -Force
foreach ($file in 'README.md', 'CHANGELOG.md') {
  Copy-Item (Join-Path $PSScriptRoot $file) $dist -Force
}
Copy-Item (Join-Path $PSScriptRoot 'docs') $dist -Recurse -Force
Copy-Item (Join-Path $PSScriptRoot 'assets\fonts\OFL.txt') (Join-Path $dist 'LICENSE-Geist-font.txt') -Force
# OpenTabletDriver, for its output mode. It needs the .NET 10 runtime, and is LGPL-3.0: its source
# is at https://github.com/OpenTabletDriver/OpenTabletDriver, at the commit build.ps1 names.
$otd = Join-Path $dist 'otd'
Copy-Item (Join-Path $PSScriptRoot 'target\otd') $otd -Recurse
Copy-Item (Join-Path $PSScriptRoot 'tools\otd-src\LICENSE') (Join-Path $otd 'LICENSE-OpenTabletDriver.txt') -Force
"Ready: $dist (osu!tab $version)"

if ($Zip) {
  $name = "osu-tab-$version-windows-x64.zip"
  $archive = Join-Path $PSScriptRoot "dist\$name"
  Remove-Item $archive -ErrorAction SilentlyContinue
  Compress-Archive -Path $dist -DestinationPath $archive -CompressionLevel Optimal
  $hash = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLower()
  "$hash  $name" | Set-Content (Join-Path $PSScriptRoot "dist\$name.sha256") -Encoding ascii
  "Zipped: $archive"
  "SHA-256: $hash"
}
