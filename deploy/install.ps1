$ErrorActionPreference = "Stop"

$Repo = if ($env:QUOD_REPO) { $env:QUOD_REPO } else { "EricTianC/quod" }
$Version = if ($env:QUOD_VERSION) { $env:QUOD_VERSION } else { "latest" }
$InstallDir = if ($env:QUOD_INSTALL_DIR) { $env:QUOD_INSTALL_DIR } else { Join-Path $HOME ".local\bin" }

$Arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
switch ($Arch) {
  "X64" { $Target = "x86_64-pc-windows-msvc" }
  "Amd64" { $Target = "x86_64-pc-windows-msvc" }
  default {
    throw "Unsupported Windows architecture for prebuilt quod CLI: $Arch (use WSL2 or build from source with Cargo)."
  }
}

$Asset = "quod-$Target.zip"
if ($Version -eq "latest") {
  $BaseUrl = "https://github.com/$Repo/releases/latest/download"
} else {
  $BaseUrl = "https://github.com/$Repo/releases/download/$Version"
}

$TmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ("quod-install-" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $TmpDir -Force | Out-Null

try {
  $ArchivePath = Join-Path $TmpDir $Asset
  Write-Host "[quod] Downloading $Asset from $BaseUrl..."
  Invoke-WebRequest -Uri "$BaseUrl/$Asset" -OutFile $ArchivePath -UseBasicParsing

  $SumsPath = Join-Path $TmpDir "SHA256SUMS"
  try {
    Invoke-WebRequest -Uri "$BaseUrl/SHA256SUMS" -OutFile $SumsPath -UseBasicParsing
    $ExpectedLine = Get-Content $SumsPath | Where-Object { $_ -match "\b$([regex]::Escape($Asset))$" } | Select-Object -First 1
    if ($ExpectedLine) {
      $ExpectedHash = ($ExpectedLine -split "\s+")[0].ToLowerInvariant()
      $ActualHash = (Get-FileHash -Path $ArchivePath -Algorithm SHA256).Hash.ToLowerInvariant()
      if ($ActualHash -ne $ExpectedHash) {
        throw "[quod] Checksum mismatch for $Asset: expected $ExpectedHash, got $ActualHash"
      }
      Write-Host "[quod] Verified SHA-256 checksum."
    }
  } catch {
    if ($_.Exception.Message -like "*Checksum mismatch*") {
      throw
    }
  }

  $ExtractDir = Join-Path $TmpDir "extract"
  Expand-Archive -Path $ArchivePath -DestinationPath $ExtractDir -Force
  $BinSrc = Join-Path $ExtractDir "quod.exe"
  if (-not (Test-Path $BinSrc)) {
    throw "[quod] Archive $Asset did not contain expected 'quod.exe' binary."
  }

  New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
  $BinDst = Join-Path $InstallDir "quod.exe"
  Copy-Item -Path $BinSrc -Destination $BinDst -Force
  Write-Host "[quod] Installed quod.exe to $BinDst"

  $SessionPaths = $env:PATH -split ";"
  if ($SessionPaths -notcontains $InstallDir) {
    $env:PATH = "$InstallDir;$env:PATH"
  }

  $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
  $UserParts = if ($UserPath) { $UserPath -split ";" | Where-Object { $_ -ne "" } } else { @() }
  if ($UserParts -notcontains $InstallDir) {
    $NewUserPath = (@($InstallDir) + $UserParts) -join ";"
    [Environment]::SetEnvironmentVariable("Path", $NewUserPath, "User")
    Write-Host "[quod] Added $InstallDir to your User PATH (open a new terminal if needed)."
  }

  & $BinDst --version

  if (-not (Get-Command lake -ErrorAction SilentlyContinue)) {
    Write-Host "[quod] Note: Lean 4 ('lake') was not found on PATH."
    Write-Host "       Install elan + Lean 4 if you plan to run local proof verification:"
    Write-Host "       https://github.com/leanprover/elan#windows"
  }
} finally {
  Remove-Item -Path $TmpDir -Recurse -Force -ErrorAction SilentlyContinue
}
