#Requires -Version 7.0
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Paths provided by the GitHub-hosted windows-2022 image. Fail explicitly if
# its software layout changes instead of silently selecting an unrelated tool.
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
rustup target add --toolchain stable x86_64-pc-windows-msvc i686-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw 'Failed to install Rust build targets.' }
