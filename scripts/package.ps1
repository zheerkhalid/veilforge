# Builds the release EXE, the portable ZIP and (if Inno Setup is installed) VeilForge-Setup.exe.
# Usage: powershell -ExecutionPolicy Bypass -File scripts\package.ps1
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$dist = Join-Path $root "dist"
New-Item -ItemType Directory -Force $dist | Out-Null

# The build script renders the icon into OUT_DIR; copy the newest one for the installer.
$ico = Get-ChildItem target\release\build -Recurse -Filter veilforge.ico | Sort-Object LastWriteTime -Descending | Select-Object -First 1
Copy-Item $ico.FullName target\veilforge.ico -Force

# Portable: the EXE, the licence texts and the flag that keeps settings next to the EXE.
$stage = Join-Path $dist "portable"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item target\release\VeilForge.exe $stage
Copy-Item LICENSE (Join-Path $stage "LICENSE.txt")
Copy-Item docs\HOW-IT-WORKS.md (Join-Path $stage "HOW-IT-WORKS.txt")
Set-Content -Path (Join-Path $stage "portable.flag") -Encoding ASCII -Value "VeilForge keeps its settings in this folder while this file is here."
$zip = Join-Path $dist "VeilForge-$version-portable-x64.zip"
Remove-Item $zip -ErrorAction SilentlyContinue
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip
Remove-Item -Recurse -Force $stage
Copy-Item target\release\VeilForge.exe (Join-Path $dist "VeilForge.exe") -Force

$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if ($iscc) {
    & $iscc "/DAppVersion=$version" installer\VeilForge.iss
    if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }
} else {
    Write-Warning "Inno Setup 6 not found: skipping VeilForge-Setup.exe (winget install JRSoftware.InnoSetup)"
}
$size = (Get-Item target\release\VeilForge.exe).Length / 1MB
Write-Host ("VeilForge.exe {0:N2} MB" -f $size)
Get-ChildItem $dist -File | Format-Table Name, @{n="MB";e={"{0:N2}" -f ($_.Length/1MB)}}
