# Mirrors D:\code\codex\xcode.ps1 line-for-line, with two changes:
#
#   1. The cargo workspace is the `codex/` git submodule that ships
#      inside this repo (https://github.com/xuezchuang/codex, branch
#      `codeforge`) rather than the standalone `D:\code\codex`
#      checkout that hosts `xcode`.
#
#   2. The config home is $HOME/.codeforge, NOT $HOME/.xcode, so cf
#      is fully independent of the standalone `xcode` launcher.
#      The env var *name* stays XCODE_HOME because that is what the
#      codex binary actually reads (see codex-rs/utils/home-dir/src/
#      lib.rs::find_codex_home / find_codex_home_from_env). Only the
#      *value* of XCODE_HOME differs.
#
#   xcode.ps1                             cf.ps1
#   ----------                            ------
#   $repoRoot = D:\code\codex             $repoRoot = D:\code\snowAgents
#   XCODE_HOME = $HOME/.xcode             XCODE_HOME = $HOME/.codeforge
#   push to <repo>/codex-rs               push to <repo>/codex/codex-rs
#   cargo run --bin codex -- --cd $cwd  cargo run --bin codex -- --cd $cwd
#
# XCODE_NO_CD=1 -> run from the codex-rs workspace root instead of the
# caller's cwd (matches xcode's legacy fallback).

$repoRoot = Split-Path -Parent $PSCommandPath
$codexWorkspace = Join-Path $repoRoot "codex\codex-rs"
$configHome = Join-Path $HOME ".codeforge"
New-Item -ItemType Directory -Force -Path $configHome | Out-Null

$previousConfigHome = [Environment]::GetEnvironmentVariable("XCODE_HOME", "Process")

# The directory the user invoked `cf` from. We forward it to the underlying
# `codex` binary via `--cd` so that the TUI banner's "directory:" line and all
# relative-path resolution match where the user actually started cf, rather
# than the repo's `codex-rs/` subdirectory that `cargo run` chdirs into.
#
# Set `XCODE_NO_CD=1` to opt out and keep the legacy behavior of running from
# the codex-rs workspace.
$callerCwd = $null
if (-not [Environment]::GetEnvironmentVariable("XCODE_NO_CD", "Process")) {
    $callerCwd = (Get-Location -PSProvider FileSystem).ProviderPath
    if ($null -ne $callerCwd -and -not (Test-Path -LiteralPath $callerCwd -PathType Container)) {
        # Caller's directory vanished between invocation and here; fall back
        # to the legacy codex-rs cwd so we never start codex with no working
        # directory at all.
        $callerCwd = $null
    }
}

if (-not (Test-Path -LiteralPath $codexWorkspace -PathType Container)) {
    Write-Host ("cf: codex submodule workspace not found at " + $codexWorkspace)
    Write-Host "Run `git submodule update --init` in the repo root to fetch it."
    exit 1
}

try {
    $env:XCODE_HOME = $configHome

    Push-Location $codexWorkspace
    try {
        if ($null -ne $callerCwd) {
            # Place --cd BEFORE @args so that an explicit user `--cd` in @args
            # still wins (clap applies last-wins for repeated options).
            cargo run --bin codex -- --cd $callerCwd @args
        } else {
            cargo run --bin codex -- @args
        }
    } finally {
        Pop-Location
    }
} finally {
    if ($null -eq $previousConfigHome) {
        Remove-Item Env:\XCODE_HOME -ErrorAction SilentlyContinue
    } else {
        $env:XCODE_HOME = $previousConfigHome
    }
}
