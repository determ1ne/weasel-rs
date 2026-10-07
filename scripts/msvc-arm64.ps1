function Save-ProcessEnvironment {
    $snapshot = [Collections.Generic.Dictionary[string, string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in [Environment]::GetEnvironmentVariables('Process').GetEnumerator()) {
        $snapshot[[string]$entry.Key] = [string]$entry.Value
    }
    $snapshot
}

function Restore-ProcessEnvironment {
    param(
        [Parameter(Mandatory)]
        [Collections.Generic.Dictionary[string, string]]$Snapshot
    )

    foreach ($name in @([Environment]::GetEnvironmentVariables('Process').Keys)) {
        if (-not $Snapshot.ContainsKey([string]$name)) {
            # On current .NET, SetEnvironmentVariable(name, $null, Process)
            # leaves an empty entry; the PowerShell provider actually removes it.
            Remove-Item -LiteralPath ("Env:\{0}" -f $name) -ErrorAction SilentlyContinue
        }
    }
    foreach ($entry in $Snapshot.GetEnumerator()) {
        [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, 'Process')
    }
}

function Import-MsvcArm64Environment {
    [CmdletBinding()]
    param()

    $inheritedPathEntries = @($env:Path -split ';' |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    $vswhereCandidates = @(
        (Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'),
        (Join-Path $env:ProgramFiles 'Microsoft Visual Studio\Installer\vswhere.exe')
    )
    $vswhere = $vswhereCandidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
    if (-not $vswhere) { throw 'Visual Studio Installer (vswhere.exe) was not found.' }
    $installation = (& $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.ARM64 -property installationPath | Select-Object -First 1)
    if (-not $installation) { throw 'Visual Studio ARM64/ARM64EC C++ build tools are not installed.' }
    $vsDevCmd = Join-Path $installation 'Common7\Tools\VsDevCmd.bat'
    if (-not (Test-Path -LiteralPath $vsDevCmd -PathType Leaf)) { throw "Missing $vsDevCmd" }

    $hostArchitecture = if ([Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture -eq [Runtime.InteropServices.Architecture]::Arm64) {
        'arm64'
    } else {
        'amd64'
    }
    $environment = & $env:ComSpec /d /s /c "`"$vsDevCmd`" -no_logo -arch=arm64 -host_arch=$hostArchitecture >nul && set"
    if ($LASTEXITCODE -ne 0) { throw 'Unable to initialize the MSVC ARM64 developer environment.' }
    $variables = [Collections.Generic.Dictionary[string, string]]::new([StringComparer]::OrdinalIgnoreCase)
    $spellings = [Collections.Generic.Dictionary[string, string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($line in $environment) {
        $separator = $line.IndexOf('=')
        if ($separator -gt 0) {
            $name = $line.Substring(0, $separator)
            $value = $line.Substring($separator + 1)
            # A PowerShell parent commonly contributes `Path`, while VsDevCmd
            # writes a new `PATH`. `cmd set` can expose both; retain the exact
            # uppercase spelling so the stale inherited value cannot win.
            if (-not $variables.ContainsKey($name) -or $name -ceq $name.ToUpperInvariant()) {
                $variables[$name] = $value
                $spellings[$name] = $name
            }
        }
    }
    foreach ($name in $variables.Keys) {
        $canonicalName = if ($name -ieq 'PATH') { 'Path' } else { $spellings[$name] }
        [Environment]::SetEnvironmentVariable($canonicalName, $variables[$name], 'Process')
    }
    # VsDevCmd must lead PATH so Cargo picks the ARM64 compiler, but it may omit
    # user-scoped tools (for example a version-managed Node installation).
    # Preserve those inherited entries at the tail for later build stages.
    $developerPathEntries = @($env:Path -split ';' |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    foreach ($entry in $inheritedPathEntries) {
        if ($entry -notin $developerPathEntries) {
            $developerPathEntries += $entry
        }
    }
    $env:Path = $developerPathEntries -join ';'
    if (-not (Get-Command cl.exe -CommandType Application -ErrorAction SilentlyContinue)) {
        throw 'The MSVC ARM64 environment did not provide cl.exe.'
    }
}
