# Build a release and collect everything needed on the target machine into dist\nya-client.
$ErrorActionPreference = 'Stop'
$repo = Resolve-Path (Join-Path $PSScriptRoot '..')
$ffmpeg = if ($env:NYA_FFMPEG_DIR) { $env:NYA_FFMPEG_DIR } else { Join-Path $repo '..\third_party\ffmpeg' }
Push-Location $repo
try {
    # cargo writes progress to stderr; Windows PowerShell treats that as an error under 'Stop'.
    $ErrorActionPreference = 'Continue'
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
    $ErrorActionPreference = 'Stop'
} finally { Pop-Location }
$out = Join-Path $repo 'dist\nya-client'
if (Test-Path $out) { Remove-Item $out -Recurse -Force }
New-Item -ItemType Directory -Force $out | Out-Null
Copy-Item (Join-Path $repo '..\target\release\nya-client.exe') $out
foreach ($dll in 'avcodec-62.dll', 'avutil-60.dll', 'swresample-6.dll') {
    Copy-Item (Join-Path $ffmpeg "bin\$dll") $out
}
Copy-Item (Join-Path $repo 'README.md') $out
# Offline installer for USB passthrough (used before downloading; see drivers-README.txt).
$usbipd = Join-Path $repo '..\third_party\drivers\usbipd-win_5.3.0_x64.msi'
if (Test-Path $usbipd) {
    New-Item -ItemType Directory -Force (Join-Path $out 'drivers') | Out-Null
    Copy-Item $usbipd (Join-Path $out 'drivers')
    Copy-Item (Join-Path $PSScriptRoot 'drivers-README.txt') (Join-Path $out 'drivers\README.txt')
} else {
    Write-Host 'no offline usbipd-win (run ..\common\scripts\fetch-drivers.ps1); one-click install will download'
}
Write-Host "packaged to $out"
