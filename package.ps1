# Builds everything and puts a ready-to-run copy in dist\osutab. That folder can be moved anywhere.
# It includes Google's adb, whose terms don't allow redistributing it, so keep the folder for your
# own use rather than publishing it.
$ErrorActionPreference = 'Stop'
& (Join-Path $PSScriptRoot 'build.ps1')
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$dist = Join-Path $PSScriptRoot 'dist\osutab'
New-Item -ItemType Directory -Force (Join-Path $dist 'platform-tools') | Out-Null
Copy-Item (Join-Path $PSScriptRoot 'target\release\osutab.exe') $dist -Force
Copy-Item (Join-Path $PSScriptRoot 'README.md') $dist -Force
Copy-Item (Join-Path $PSScriptRoot 'assets\fonts\OFL.txt') (Join-Path $dist 'LICENSE-Geist-font.txt') -Force
$adb = Join-Path $PSScriptRoot 'tools\platform-tools'
foreach ($file in 'adb.exe', 'AdbWinApi.dll', 'AdbWinUsbApi.dll', 'NOTICE.txt') {
  Copy-Item (Join-Path $adb $file) (Join-Path $dist 'platform-tools') -Force
}
# OpenTabletDriver, for its output mode. It needs the .NET 10 runtime, and is LGPL-3.0.
$otd = Join-Path $dist 'otd'
Remove-Item -Recurse -Force $otd -ErrorAction SilentlyContinue
Copy-Item (Join-Path $PSScriptRoot 'target\otd') $otd -Recurse
Copy-Item (Join-Path $PSScriptRoot 'tools\otd-src\LICENSE') (Join-Path $otd 'LICENSE-OpenTabletDriver.txt') -Force
"Ready: $dist"
