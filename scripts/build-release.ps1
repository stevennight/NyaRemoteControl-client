# Release build of the client: fetches missing third-party pieces, builds and
# packages (scripts\package.ps1, into dist\nya-client), then writes into release\:
#   NyaRemoteControl-Client_<version>_x64-setup.exe     installer (NSIS)
#   NyaRemoteControl-Client_<version>_windows_x64.zip   portable zip
# each with a .sha256 next to it.
#
#   .\scripts\build-release.ps1                 # local build of the current VERSION
#   .\scripts\build-release.ps1 -Tag v0.2.0     # CI: also check the tag matches VERSION
param(
    [string]$Tag = '',
    [switch]$SkipInstaller
)
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
. (Join-Path $repo '..\common\scripts\release-lib.ps1')

$version = Test-NyaVersion $repo @('Cargo.toml') $Tag
Write-Host "== NyaRemoteControl Client $version" -ForegroundColor Cyan
Initialize-NyaThirdParty
& (Join-Path $PSScriptRoot 'package.ps1')

New-NyaReleaseFiles -Repo $repo -Name 'NyaRemoteControl-Client' -Version $version `
    -Stage (Join-Path $repo 'dist\nya-client') -Nsi (Join-Path $repo 'installer\nya-client.nsi') `
    -Icon (Resolve-Path (Join-Path $repo '..\common\assets\client.ico')).Path -SkipInstaller:$SkipInstaller
