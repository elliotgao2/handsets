# Handsets installer — Windows (PowerShell 5+).
#
#   iwr -useb https://raw.githubusercontent.com/elliotgao2/handsets/main/install.ps1 | iex
#
# Env overrides (all optional):
#   $env:HANDSETS_VERSION  = 'vX.Y.Z'           # pin a release (default: latest)
#   $env:HANDSETS_DIR      = "$HOME\.handsets"  # install location
#   $env:HANDSETS_REPO     = 'elliotgao2/handsets'

$ErrorActionPreference = 'Stop'

$Repo    = if ($env:HANDSETS_REPO) { $env:HANDSETS_REPO } else { 'elliotgao2/handsets' }
$Dir     = if ($env:HANDSETS_DIR)  { $env:HANDSETS_DIR  } else { Join-Path $HOME '.handsets' }

# ---------- detect arch ----------

$archRaw = $env:PROCESSOR_ARCHITECTURE
if (-not $archRaw) { $archRaw = (Get-CimInstance Win32_Processor).Architecture }
switch -Regex ($archRaw) {
  'AMD64|x86_64|9'  { $Arch = 'x86_64' }
  default { throw "Unsupported Windows arch: $archRaw (only x86_64 is published today)" }
}

$Asset = "handsets-windows-$Arch.zip"

# ---------- resolve version ----------

$Version = $env:HANDSETS_VERSION
if (-not $Version) {
  $api = "https://api.github.com/repos/$Repo/releases/latest"
  $rel = Invoke-RestMethod -UseBasicParsing -Uri $api
  $Version = $rel.tag_name
  if (-not $Version) { throw "Could not resolve latest release for $Repo" }
}

$Url     = "https://github.com/$Repo/releases/download/$Version/$Asset"
$SumsUrl = "$Url.sha256"

Write-Host "Installing handsets $Version (windows/$Arch) -> $Dir"

# ---------- download ----------

$Tmp = New-Item -ItemType Directory -Force -Path (Join-Path $env:TEMP "handsets-$([System.Guid]::NewGuid())")
try {
  $ZipPath = Join-Path $Tmp $Asset
  Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $ZipPath

  try {
    $SumPath = Join-Path $Tmp "$Asset.sha256"
    Invoke-WebRequest -UseBasicParsing -Uri $SumsUrl -OutFile $SumPath
    $expected = ((Get-Content $SumPath -Raw) -split '\s+')[0].ToLower()
    $actual   = (Get-FileHash $ZipPath -Algorithm SHA256).Hash.ToLower()
    if ($expected -and $expected -ne $actual) {
      throw "Checksum mismatch (expected $expected, got $actual)"
    }
  } catch [System.Net.WebException] {
    Write-Warning "No checksum file at $SumsUrl (skipping verify)"
  }

  # ---------- extract ----------

  New-Item -ItemType Directory -Force -Path $Dir | Out-Null
  Get-ChildItem -Path $Dir -Include 'hs.exe','hs.jar','LICENSE','VERSION' -File `
    -ErrorAction SilentlyContinue | Remove-Item -Force

  # Tarballs ship the binary under a top-level handsets/ dir; the zip uses
  # the same convention. Expand and flatten.
  $stage = Join-Path $Tmp 'stage'
  Expand-Archive -Path $ZipPath -DestinationPath $stage -Force
  $top = Get-ChildItem $stage | Where-Object PSIsContainer | Select-Object -First 1
  $src = if ($top) { $top.FullName } else { $stage }
  Get-ChildItem -Path $src -File | Copy-Item -Destination $Dir -Force

  Set-Content -Path (Join-Path $Dir 'VERSION') -Value $Version -NoNewline
} finally {
  Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}

# ---------- PATH ----------

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not $userPath) { $userPath = '' }
$parts = $userPath -split ';' | Where-Object { $_ -ne '' }
if ($parts -notcontains $Dir) {
  $newPath = if ($userPath) { "$userPath;$Dir" } else { $Dir }
  [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
  Write-Host "Added $Dir to user Path (open a new shell to pick it up)"
} else {
  Write-Host "$Dir already on user Path"
}

Write-Host ""
Write-Host "  installed:  $Dir\hs.exe"
Write-Host "  daemon jar: $Dir\hs.jar"
Write-Host ""
Write-Host "Next:  hs use      # connect a device and start the daemon"
