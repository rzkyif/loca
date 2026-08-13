# Provisions a Windows-on-ARM guest to build loca-core + helpers + speech for
# BOTH Windows triples (aarch64- and x86_64-pc-windows-msvc), driven by
# scripts/release/drivers/windows.ts.
#
# How one ARM64 VM covers both arches:
#   - core: the arm64-native deno compiles aarch64 NATIVELY (deno compile has no
#     win-arm64 cross target, but compiles it natively) and cross-compiles x64
#     via --target x86_64-pc-windows-msvc.
#   - helpers + speech: cargo builds aarch64 natively and cross-builds x64
#     (vcvars arm64_amd64).
#   - speech lib: sherpa-onnx-sys's download map omits win-arm64, so we stage the
#     upstream win-arm64 static lib and the build points SHERPA_ONNX_LIB_DIR at
#     it; the x64 build uses the win-x64 lib the crate downloads itself.
#
# Run ONCE in an ELEVATED PowerShell inside the guest:
#   powershell -NoProfile -ExecutionPolicy Bypass -File windows-provision.ps1
#
# Re-running is safe (winget skips already-installed packages).

$ErrorActionPreference = "Stop"

# Pins. Deno: the build runtime (its denort is embedded into the core binary).
$DenoVersion = "2.9.0"
# Sherpa: must match packages/loca-core-speech/Cargo.toml's sherpa-onnx version.
$SherpaVersion = "1.13.3"
$SherpaLibName = "sherpa-onnx-v$SherpaVersion-win-arm64-static-MT-Release-lib"
$SherpaRoot = "C:\sherpa-onnx-libs"
$DenoDir = "C:\deno"

function Refresh-Path {
  $machine = [System.Environment]::GetEnvironmentVariable("Path", "Machine")
  $user = [System.Environment]::GetEnvironmentVariable("Path", "User")
  $env:Path = "$machine;$user"
}

function Prepend-MachinePath {
  param([string]$Dir)
  $mp = [Environment]::GetEnvironmentVariable("Path", "Machine")
  if ($mp -notlike "*$Dir*") {
    [Environment]::SetEnvironmentVariable("Path", "$Dir;$mp", "Machine")
  }
}

# Always pin --source winget: the msstore source can fail cert validation
# (0x8a15005e) and, when a package exists in both sources, winget refuses to pick.
# winget returns non-zero when a package is already installed, so we don't treat
# the exit code as fatal here; the tool-presence checks at the end catch real
# failures.
function Winget-Install {
  param([string]$Id, [string]$Override)
  $a = @(
    "install", "--id", $Id, "-e", "--source", "winget", "--silent",
    "--accept-source-agreements", "--accept-package-agreements"
  )
  if ($Override) { $a += @("--override", $Override) }
  Write-Host "    winget install $Id"
  winget @a
}

Write-Host "==> VS Build Tools (MSVC + ARM64 C++ tools, incl. x64 cross)"
Winget-Install "Microsoft.VisualStudio.2022.BuildTools" "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.VC.Tools.ARM64 --includeRecommended"

Write-Host "==> CMake + Git"
Winget-Install "Kitware.CMake"
Winget-Install "Git.Git"

Write-Host "==> NSIS (makensis, for the Core native installer)"
# makensis compiles the hand-authored Core NSIS installer (core-installers.ts).
# The Tauri client NSIS is built by Tauri's own bundled makensis; this is for the
# standalone Core package.
Winget-Install "NSIS.NSIS"

Write-Host "==> rustup + toolchain 1.96.0 + both windows targets"
Winget-Install "Rustlang.Rustup"
Refresh-Path
if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
  throw "rustup not found after install. Open a NEW elevated shell and re-run, or install rustup manually from https://rustup.rs"
}
rustup toolchain install 1.96.0
rustup default 1.96.0
# aarch64 is the host (native); x86_64 is the cross target.
rustup target add aarch64-pc-windows-msvc x86_64-pc-windows-msvc

Write-Host "==> LLVM/clang"
# sherpa-onnx-sys pulls ring + bzip2-sys (its downloader's TLS + bzip2), which
# C-compile with clang. winget's silent install does not add LLVM to PATH.
Winget-Install "LLVM.LLVM"
Prepend-MachinePath "C:\Program Files\LLVM\bin"
Refresh-Path

Write-Host "==> deno (arm64-native build $DenoVersion)"
# Must be the arm64 build: `deno compile` (no --target) then produces an arm64
# core, and it still cross-compiles x64 via --target. The default install script
# would fetch the x64 build (emulated), which can't produce an arm64 core.
New-Item -ItemType Directory -Force -Path $DenoDir | Out-Null
$denoZip = Join-Path $env:TEMP "deno-arm64.zip"
Invoke-WebRequest "https://github.com/denoland/deno/releases/download/v$DenoVersion/deno-aarch64-pc-windows-msvc.zip" -OutFile $denoZip
tar -xf $denoZip -C $DenoDir
Prepend-MachinePath $DenoDir
Refresh-Path

Write-Host "==> sherpa-onnx win-arm64 static lib (speech, aarch64 build)"
New-Item -ItemType Directory -Force -Path $SherpaRoot | Out-Null
$archive = Join-Path $SherpaRoot "$SherpaLibName.tar.bz2"
$url = "https://github.com/k2-fsa/sherpa-onnx/releases/download/v$SherpaVersion/$SherpaLibName.tar.bz2"
if (-not (Test-Path $archive)) {
  Write-Host "    downloading $url"
  Invoke-WebRequest -Uri $url -OutFile $archive
}
tar -xf $archive -C $SherpaRoot
$coreLib = Get-ChildItem -Recurse -Path $SherpaRoot -Filter "sherpa-onnx-core.lib" |
  Select-Object -First 1
if (-not $coreLib) { throw "sherpa-onnx-core.lib not found under $SherpaRoot after extract" }
# The aarch64 build reads this; the driver sets SHERPA_ONNX_LIB_DIR from it for
# the arm64 triple and leaves it unset for x64 (crate downloads the win-x64 lib).
[System.Environment]::SetEnvironmentVariable("SHERPA_ONNX_LIB_DIR_ARM64", $coreLib.DirectoryName, "Machine")
Write-Host "    SHERPA_ONNX_LIB_DIR_ARM64 = $($coreLib.DirectoryName)"

Write-Host ""
Write-Host "==> versions"
deno --version
rustc --version
clang --version | Select-Object -First 1
Write-Host ""
Write-Host "Provisioning done. Open a NEW shell so PATH + env changes take effect."
