#Requires -Version 7.0
[CmdletBinding()]
param(
    [ValidateSet('x64', 'arm64')]
    [string]$Architecture = 'x64'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Paths provided by the GitHub-hosted x64 and ARM64 Windows images. Fail
# explicitly if their software layout changes instead of selecting another tool.
$toolDirectories = @(
    (Join-Path ${env:ProgramFiles(x86)} 'NSIS'),
    (Join-Path $env:ProgramFiles '7-Zip')
)
foreach ($directory in $toolDirectories) {
    if (-not (Test-Path -LiteralPath $directory -PathType Container)) {
        throw "Required build tools not found: $directory"
    }
    Add-Content -LiteralPath $env:GITHUB_PATH -Value $directory
}
$clangDirectory = Join-Path $env:ProgramFiles 'LLVM\bin'
if (-not (Test-Path -LiteralPath (Join-Path $clangDirectory 'libclang.dll'))) {
    throw "libclang.dll not found in $clangDirectory"
}
Add-Content -LiteralPath $env:GITHUB_ENV -Value "LIBCLANG_PATH=$clangDirectory"

rustup toolchain install stable --profile minimal
if ($LASTEXITCODE -ne 0) { throw 'Failed to install Rust stable.' }
rustup default stable
if ($LASTEXITCODE -ne 0) { throw 'Failed to select Rust stable.' }
$targets = if ($Architecture -eq 'arm64') {
    @('aarch64-pc-windows-msvc', 'arm64ec-pc-windows-msvc', 'i686-pc-windows-msvc', 'wasm32-unknown-unknown')
} else {
    @('x86_64-pc-windows-msvc', 'i686-pc-windows-msvc', 'wasm32-unknown-unknown')
}
rustup target add --toolchain stable @targets
if ($LASTEXITCODE -ne 0) { throw 'Failed to install Rust build targets.' }
