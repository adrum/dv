# Install dv on Windows.
#
#   powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/adrum/dv/main/install.ps1 | iex"
#
# The Windows counterpart of install.sh, and it keeps the same promises.
# Downloads the newest published dv, verifies it against the sha256 published
# beside it, and puts dv.exe on disk. Nothing else — it does not edit PATH or
# your PowerShell profile, because a script that rewrites your environment
# from a pipe is a worse trade than a line you add yourself.
#
# Written for Windows PowerShell 5.1 as well as PowerShell 7, since 5.1 is
# what every Windows machine has before anything else is installed.

& {
    $ErrorActionPreference = 'Stop'
    # Invoke-WebRequest redraws its progress bar per chunk on 5.1, which makes
    # a download many times slower than the network.
    $ProgressPreference = 'SilentlyContinue'

    # `iex` runs this in the caller's session, so `exit` would close their
    # window. Failures throw instead, which stops the script and leaves the
    # shell open.
    function Fail([string]$message) { throw "dv install: $message" }

    $repo = if ($env:DV_REPO) { $env:DV_REPO } else { 'adrum/dv' }
    $baseUrl = if ($env:DV_INSTALL_BASE_URL) { $env:DV_INSTALL_BASE_URL.TrimEnd('/') } `
               else { "https://github.com/$repo/releases/latest/download" }
    $binDir = if ($env:DV_BIN_DIR) { $env:DV_BIN_DIR } `
              else { Join-Path $env:USERPROFILE '.local\bin' }

    # https is pinned for the published location. An explicit
    # DV_INSTALL_BASE_URL is someone deliberately pointing at a mirror or a
    # test server, so it follows the scheme they chose — but says so.
    if (-not $baseUrl.StartsWith('https://')) {
        Write-Warning "dv install: $baseUrl is not https"
    }
    # Windows PowerShell 5.1 does not offer TLS 1.2 unless asked to.
    [Net.ServicePointManager]::SecurityProtocol =
        [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    # PowerShell 7 runs on other systems too; install.sh is the script there.
    if ($PSVersionTable.PSEdition -eq 'Core' -and -not $IsWindows) {
        Fail 'this installer is for Windows — use install.sh on macOS and Linux'
    }
    # Asset names use .NET runtime identifiers, the same names dv itself uses
    # for SDK downloads. Microsoft publishes native Arm64 SDKs, so Arm gets a
    # native dv too.
    $rid = switch ($env:PROCESSOR_ARCHITECTURE) {
        'AMD64' { 'win-x64' }
        'ARM64' { 'win-arm64' }
        default { Fail "unsupported architecture $($env:PROCESSOR_ARCHITECTURE) — dv builds for x64 and Arm64" }
    }
    if (-not (Get-Command tar.exe -ErrorAction SilentlyContinue)) {
        Fail 'tar.exe is required (it ships with Windows 10 1803 and later)'
    }

    # The unversioned name is published by the release workflow so this script
    # needs no way to discover the current version.
    $archive = "dv-$rid.tar.gz"
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("dv-install-" + [Guid]::NewGuid())
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        Write-Host "dv install: fetching $archive"
        try {
            Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/$archive" -OutFile (Join-Path $tmp $archive)
        } catch {
            Fail "could not download $baseUrl/$archive"
        }
        try {
            Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/$archive.sha256" -OutFile (Join-Path $tmp "$archive.sha256")
        } catch {
            Fail "could not download the checksum for $archive — refusing to install unverified"
        }

        # Verification fails closed: no checksum, a malformed one, or a
        # mismatch all stop the install.
        $expected = ((Get-Content -Raw (Join-Path $tmp "$archive.sha256")).Trim() -split '\s+')[0].ToLowerInvariant()
        if ($expected -notmatch '^[0-9a-f]{64}$') {
            Fail "the published checksum for $archive is not a sha256 — refusing to install"
        }
        $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp $archive)).Hash.ToLowerInvariant()
        if ($expected -ne $actual) {
            Fail "checksum mismatch for $archive (expected $expected, got $actual) — nothing was installed"
        }

        & tar.exe -xzf (Join-Path $tmp $archive) -C $tmp
        if ($LASTEXITCODE -ne 0) { Fail "could not extract $archive" }
        $binary = Get-ChildItem -Path $tmp -Recurse -File -Filter 'dv.exe' | Select-Object -First 1
        if (-not $binary) { Fail "$archive did not contain dv.exe" }

        New-Item -ItemType Directory -Force -Path $binDir | Out-Null
        $target = Join-Path $binDir 'dv.exe'
        $incoming = Join-Path $binDir 'dv.exe.incoming'
        # Install by rename, so a half-copied binary is never left on PATH.
        # A running dv.exe cannot be replaced, but it can be renamed — the
        # same step-aside dv self update takes.
        Copy-Item -Force $binary.FullName $incoming
        $old = Join-Path $binDir 'dv.old'
        Remove-Item -Force -ErrorAction SilentlyContinue $old
        try {
            Move-Item -Force $incoming $target
        } catch {
            Move-Item -Force $target $old
            Move-Item -Force $incoming $target
        }
        Remove-Item -Force -ErrorAction SilentlyContinue $old

        Write-Host "dv install: installed $(& $target --version) to $target"
    } finally {
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $tmp
    }

    $onPath = ($env:PATH -split ';') | Where-Object { $_.TrimEnd('\') -ieq $binDir.TrimEnd('\') }
    if (-not $onPath) {
        Write-Host ''
        Write-Host "$binDir is not on your PATH. Add it to your user PATH"
        Write-Host '(Settings > "Edit environment variables for your account"), or for this session:'
        Write-Host "  `$env:PATH = `"$binDir;`$env:PATH`""
    }

    Write-Host ''
    Write-Host 'Next:'
    Write-Host '  dv install lts                                         # get an SDK'
    Write-Host '  dv init powershell | Out-String | Invoke-Expression    # add this to $PROFILE'
    Write-Host '  dv doctor                                              # confirm it worked'
}
