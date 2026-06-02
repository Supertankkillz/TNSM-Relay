# bump-version.ps1  (TNSM Relay)
# -------------------------------------------------------------------------
# One command to cut a relay release:
#   1. Bump the version in BOTH relay files (tauri.conf.json + Cargo.toml).
#   2. Build the exe LOCALLY (cargo tauri build) -> you get the artifact now.
#   3. Commit + tag + push to git (source backup / version history).
#
# Usage:
#   .\scripts\bump-version.ps1 0.1.1
#   .\scripts\bump-version.ps1 0.1.1 -NoBuild      # bump + git only, skip build
#   .\scripts\bump-version.ps1 0.1.1 -NoGit        # bump + build only, no push
#   .\scripts\bump-version.ps1 0.1.1 -Notes "Fixed the foo"
#
# Version must be strict MAJOR.MINOR.PATCH (e.g. 0.1.1). The build produces
# the exe under gui\src-tauri\target\release\bundle\ ; the path is printed at
# the end.
# -------------------------------------------------------------------------

param(
    [Parameter(Mandatory=$true, Position=0)]
    [string]$NewVersion,
    [switch]$NoBuild,
    [switch]$NoGit,
    [string]$Notes
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

# -- Validate version (strict semver: three integer parts, no prefix/suffix) -
if ($NewVersion -notmatch '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$') {
    Write-Host ""
    Write-Host "Invalid version: $NewVersion" -ForegroundColor Red
    Write-Host "Expected MAJOR.MINOR.PATCH with integer parts." -ForegroundColor Red
    Write-Host "Examples:  0.1.1   1.0.0   2.10.0" -ForegroundColor Yellow
    Write-Host "Rejected:  v0.1.1  0.1  0.1.1.2  0.1.1-beta  01.2.3" -ForegroundColor Yellow
    exit 1
}

$confPath  = "gui\src-tauri\tauri.conf.json"
$cargoPath = "gui\src-tauri\Cargo.toml"
if (-not (Test-Path $confPath))  { Write-Error "Cannot find $confPath. Run from the repo root (the folder that contains 'gui' and 'scripts')." }
if (-not (Test-Path $cargoPath)) { Write-Error "Cannot find $cargoPath." }

# -- Read current version (single source of truth = tauri.conf.json) ---------
$currentVersion = ([System.IO.File]::ReadAllText((Resolve-Path $confPath).Path) | ConvertFrom-Json).version

if ($currentVersion -eq $NewVersion) {
    Write-Host "Already at $NewVersion in config; proceeding (will rebuild/republish)." -ForegroundColor Yellow
} else {
    $curV = [Version]$currentVersion
    $newV = [Version]$NewVersion
    if ($newV -lt $curV) {
        Write-Host ""
        Write-Host "WARNING: $NewVersion is LOWER than current $currentVersion." -ForegroundColor Yellow
        $reply = Read-Host "Continue anyway? (y/N)"
        if ($reply -ne "y" -and $reply -ne "Y") { Write-Host "Aborted." -ForegroundColor Red; exit 1 }
    }
    Write-Host ""
    Write-Host "Bumping $currentVersion -> $NewVersion" -ForegroundColor Cyan
}
Write-Host ""

# -- Safe file read (avoids Get-Content -Raw quirks; guards corruption) ------
function Read-FileText($path) {
    $resolved = (Resolve-Path $path).Path
    $info = [System.IO.FileInfo]::new($resolved)
    if ($info.Length -gt 102400) {
        Write-Host ""
        Write-Host "FATAL: $path is $($info.Length) bytes (expected <10 KB)." -ForegroundColor Red
        Write-Host "It looks corrupted (likely a prior bad encoding write)." -ForegroundColor Red
        Write-Host "Recover with:  git checkout $path  then re-run." -ForegroundColor Yellow
        Write-Error "Refusing to read corrupted file."
    }
    return [System.IO.File]::ReadAllText($resolved)
}

# -- Patch helpers (key-anchored; write UTF-8 WITHOUT BOM) -------------------
function Update-JsonVersion($path, $newVer) {
    $content = Read-FileText $path
    $pattern = '("version"\s*:\s*")([^"]+)(")'
    if ($content -notmatch $pattern) { Write-Error "${path}: no version key found." }
    $updated = [regex]::Replace($content, $pattern, ('${1}' + $newVer + '${3}'), 1)
    [System.IO.File]::WriteAllText((Resolve-Path $path), $updated, (New-Object System.Text.UTF8Encoding $false))
    Write-Host "  patched $path" -ForegroundColor Green
}
function Update-TomlVersion($path, $newVer) {
    $content = Read-FileText $path
    # (?m) + start-of-line anchor so it can't hit dependency lines like tauri = "2".
    $pattern = '(?m)^(version\s*=\s*")([^"]+)(")'
    if ($content -notmatch $pattern) { Write-Error "${path}: no top-level version line found." }
    $updated = [regex]::Replace($content, $pattern, ('${1}' + $newVer + '${3}'), 1)
    [System.IO.File]::WriteAllText((Resolve-Path $path), $updated, (New-Object System.Text.UTF8Encoding $false))
    Write-Host "  patched $path" -ForegroundColor Green
}

Update-JsonVersion $confPath  $NewVersion
Update-TomlVersion $cargoPath $NewVersion

# -- Verify both match ------------------------------------------------------
$confVer  = (Read-FileText $confPath | ConvertFrom-Json).version
$cargoVer = (Select-String -Path $cargoPath -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
Write-Host ""
Write-Host "Post-bump verification:" -ForegroundColor Cyan
Write-Host "  $confPath  : $confVer"
Write-Host "  $cargoPath : $cargoVer"
if (($confVer -ne $NewVersion) -or ($cargoVer -ne $NewVersion)) {
    Write-Error "Version mismatch after bump. Aborting before build."
}
Write-Host "Both match." -ForegroundColor Green

# -- Build locally ----------------------------------------------------------
if ($NoBuild) {
    Write-Host ""
    Write-Host "Skipping build (-NoBuild)." -ForegroundColor Yellow
} else {
    Write-Host ""
    Write-Host "Building the relay GUI exe (cargo tauri build) ..." -ForegroundColor Cyan
    Push-Location "gui"
    try {
        # Prefer the tauri CLI if present; fall back to cargo subcommand.
        & cargo tauri build
        if ($LASTEXITCODE -ne 0) { throw "cargo tauri build failed (exit $LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
    $bundleDir = "gui\src-tauri\target\release\bundle"
    Write-Host ""
    Write-Host "Build complete. Artifacts under:" -ForegroundColor Green
    Write-Host "  $repoRoot\$bundleDir" -ForegroundColor Green
    # Show the produced exe(s)/installers for convenience.
    if (Test-Path $bundleDir) {
        Get-ChildItem -Recurse -Path $bundleDir -Include *.exe,*.msi -ErrorAction SilentlyContinue |
            ForEach-Object { Write-Host "    $($_.FullName)" -ForegroundColor Gray }
    }
}

# -- Commit + tag + push ----------------------------------------------------
if ($NoGit) {
    Write-Host ""
    Write-Host "Skipping git (-NoGit). Version is bumped locally." -ForegroundColor Yellow
    exit 0
}

# Only proceed with git if this is actually a git repo with a remote.
$insideGit = $false
try { & git rev-parse --is-inside-work-tree 2>$null | Out-Null; if ($LASTEXITCODE -eq 0) { $insideGit = $true } } catch {}
if (-not $insideGit) {
    Write-Host ""
    Write-Host "Not a git repo - skipping push. (Set one up with 'git init' + a remote if you want history.)" -ForegroundColor Yellow
    exit 0
}

$commitMsg = if ($Notes) { "release v$NewVersion`n`n$Notes" } else { "release v$NewVersion" }
Write-Host ""
Write-Host "Committing + tagging v$NewVersion ..." -ForegroundColor Cyan
& git add -A
& git commit -m $commitMsg
if ($LASTEXITCODE -ne 0) { Write-Host "Nothing to commit (or commit failed); continuing to tag." -ForegroundColor Yellow }

# Create the tag. Delete a same-named local tag first ONLY if it exists, so
# re-runs don't fail with "already exists" and we don't print a scary (but
# harmless) "tag not found" error on the first run.
$existingTags = & git tag --list "v$NewVersion" 2>$null
if ($existingTags) {
    & git tag -d "v$NewVersion" 2>$null | Out-Null
}
& git tag "v$NewVersion"

& git push
& git push origin "v$NewVersion"
if ($LASTEXITCODE -ne 0) {
    Write-Host ""
    Write-Host "git push failed (no remote? not authenticated?). Local commit + tag are in place." -ForegroundColor Yellow
    exit 1
}

# -- Upload the built exe/installer to the GitHub release -------------------
# GitHub auto-creates a release from the pushed tag with "Source code" zips.
# Those are just source snapshots, NOT your app. We attach the real installer
# here so the release has a downloadable .exe. Requires the GitHub CLI (gh).
if (-not $NoBuild) {
    $ghOk = $false
    try { & gh --version 2>$null | Out-Null; if ($LASTEXITCODE -eq 0) { $ghOk = $true } } catch {}
    if (-not $ghOk) {
        Write-Host ""
        Write-Host "GitHub CLI (gh) not found - skipping exe upload." -ForegroundColor Yellow
        Write-Host "Install it (winget install GitHub.cli; gh auth login) to auto-attach the exe." -ForegroundColor Yellow
        Write-Host "Your built exe is local under gui\src-tauri\target\release\bundle\." -ForegroundColor Yellow
    } else {
        $bundle = "gui\src-tauri\target\release\bundle"
        $assets = @()
        if (Test-Path $bundle) {
            $assets += Get-ChildItem -Recurse -Path $bundle -Include *.exe,*.msi -ErrorAction SilentlyContinue |
                       Where-Object { $_.Name -like "*$NewVersion*" } |
                       ForEach-Object { $_.FullName }
        }
        if ($assets.Count -eq 0) {
            Write-Host ""
            Write-Host "No installer found under $bundle for v$NewVersion - nothing to upload." -ForegroundColor Yellow
        } else {
            Write-Host ""
            Write-Host "Uploading installer(s) to the GitHub release v$NewVersion ..." -ForegroundColor Cyan
            foreach ($a in $assets) { Write-Host "  $a" -ForegroundColor Gray }
            & gh release view "v$NewVersion" 2>$null | Out-Null
            if ($LASTEXITCODE -ne 0) {
                $notesArg = if ($Notes) { $Notes } else { "Release v$NewVersion" }
                & gh release create "v$NewVersion" --title "TNSM Relay v$NewVersion" --notes $notesArg
            }
            & gh release upload "v$NewVersion" @assets --clobber
            if ($LASTEXITCODE -eq 0) {
                Write-Host "Installer uploaded to the release." -ForegroundColor Green
            } else {
                Write-Host "Upload failed (check 'gh auth status'). Tag/commit are pushed." -ForegroundColor Yellow
            }
        }
    }
}

Write-Host ""
Write-Host "Done. v$NewVersion built locally and pushed to git." -ForegroundColor Green
