$ErrorActionPreference = "Stop"

$Repository = "https://github.com/rankupgames/Spectra"
$Version = if ($env:SPECTRA_VERSION) { $env:SPECTRA_VERSION } else { "latest" }
$InstallDir = if ($env:SPECTRA_INSTALL_DIR) {
    $env:SPECTRA_INSTALL_DIR
} else {
    Join-Path $env:LOCALAPPDATA "Programs\Spectra"
}

if (-not [Environment]::Is64BitOperatingSystem) {
    throw "Spectra requires 64-bit Windows"
}

if ($Version -eq "latest") {
    $ReleaseUrl = "$Repository/releases/latest/download"
} else {
    $Tag = if ($Version.StartsWith("v")) { $Version } else { "v$Version" }
    $ReleaseUrl = "$Repository/releases/download/$Tag"
}

$Archive = "spectra-x86_64-pc-windows-msvc.zip"
$Temporary = Join-Path ([IO.Path]::GetTempPath()) ("spectra-install-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $Temporary | Out-Null
try {
    Invoke-WebRequest "$ReleaseUrl/$Archive" -OutFile (Join-Path $Temporary $Archive)
    Invoke-WebRequest "$ReleaseUrl/SHA256SUMS" -OutFile (Join-Path $Temporary "SHA256SUMS")
    $ChecksumLine = Get-Content (Join-Path $Temporary "SHA256SUMS") |
        Where-Object { $_ -match "\s$([regex]::Escape($Archive))$" } |
        Select-Object -First 1
    if (-not $ChecksumLine) {
        throw "Release checksum is missing $Archive"
    }
    $Expected = ($ChecksumLine -split "\s+")[0].ToLowerInvariant()
    $Actual = (Get-FileHash (Join-Path $Temporary $Archive) -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($Actual -ne $Expected) {
        throw "Checksum verification failed"
    }
    Expand-Archive (Join-Path $Temporary $Archive) -DestinationPath $Temporary -Force
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Copy-Item (Join-Path $Temporary "spectra.exe") (Join-Path $InstallDir "spectra.exe") -Force
    Write-Host "Installed Spectra to $InstallDir\spectra.exe"
    Write-Host "Add $InstallDir to PATH, then run: spectra install"
} finally {
    Remove-Item -Recurse -Force $Temporary -ErrorAction SilentlyContinue
}
