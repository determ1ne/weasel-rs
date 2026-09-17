#Requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$InstallDirectory,
    [ValidateSet('Launch', 'PostInstall', 'PostUninstall')]
    [string]$Mode = 'Launch',
    [switch]$ValidateOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$stage = 'Initialize'
try {
    # Never resolve System.dll from NSIS's plugin directory. Use explicit
    # framework references and a framework working directory for CodeDOM.
    $framework = [Runtime.InteropServices.RuntimeEnvironment]::GetRuntimeDirectory()
    Set-Location -LiteralPath $framework
    [Environment]::CurrentDirectory = $framework
    $stage = 'Compile unelevated launcher'
    Add-Type -ReferencedAssemblies (Join-Path $framework 'System.dll') -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
using System.ComponentModel;

public static class Unelevated
{
    const uint PROCESS_QUERY_LIMITED_INFORMATION = 0x1000;

    const uint TOKEN_ASSIGN_PRIMARY   = 0x0001;
    const uint TOKEN_DUPLICATE        = 0x0002;
    const uint TOKEN_QUERY            = 0x0008;
    const uint TOKEN_ADJUST_DEFAULT   = 0x0080;
    const uint TOKEN_ADJUST_SESSIONID = 0x0100;
    const uint CREATE_UNICODE_ENVIRONMENT = 0x00000400;
    const uint INFINITE = 0xffffffff;

    [StructLayout(LayoutKind.Sequential)]
    struct STARTUPINFO {
        public int cb;
        public IntPtr lpReserved, lpDesktop, lpTitle;
        public uint dwX, dwY, dwXSize, dwYSize;
        public uint dwXCountChars, dwYCountChars;
        public uint dwFillAttribute, dwFlags;
        public short wShowWindow, cbReserved2;
        public IntPtr lpReserved2, hStdInput, hStdOutput, hStdError;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct PROCESS_INFORMATION {
        public IntPtr hProcess, hThread;
        public uint dwProcessId, dwThreadId;
    }

    [DllImport("user32.dll")]
    static extern IntPtr GetShellWindow();

    [DllImport("user32.dll")]
    static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);

    [DllImport("kernel32.dll", SetLastError=true)]
    static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);

    [DllImport("advapi32.dll", SetLastError=true)]
    static extern bool OpenProcessToken(
        IntPtr process, uint access, out IntPtr token);

    [DllImport("advapi32.dll", SetLastError=true)]
    static extern bool DuplicateTokenEx(
        IntPtr existingToken,
        uint desiredAccess,
        IntPtr tokenAttributes,
        int impersonationLevel,
        int tokenType,
        out IntPtr newToken);

    [DllImport("advapi32.dll",
        SetLastError=true,
        CharSet=CharSet.Unicode)]
    static extern bool CreateProcessWithTokenW(
        IntPtr token,
        uint logonFlags,
        string applicationName,
        StringBuilder commandLine,
        uint creationFlags,
        IntPtr environment,
        string currentDirectory,
        ref STARTUPINFO startupInfo,
        out PROCESS_INFORMATION processInfo);

    [DllImport("userenv.dll", SetLastError=true)]
    static extern bool CreateEnvironmentBlock(
        out IntPtr environment, IntPtr token, bool inherit);

    [DllImport("userenv.dll")]
    static extern bool DestroyEnvironmentBlock(IntPtr environment);

    [DllImport("kernel32.dll", SetLastError=true)]
    static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);

    [DllImport("kernel32.dll", SetLastError=true)]
    static extern bool GetExitCodeProcess(IntPtr process, out uint exitCode);

    [DllImport("kernel32.dll")]
    static extern bool CloseHandle(IntPtr h);

    static void Error(string where)
    {
        throw new Win32Exception(
            Marshal.GetLastWin32Error(), where);
    }

    public static uint Start(string exe, string args, bool wait)
    {
        IntPtr hp = IntPtr.Zero;
        IntPtr ht = IntPtr.Zero;
        IntPtr hd = IntPtr.Zero;
        IntPtr environment = IntPtr.Zero;

        try
        {
            // Explorer PID
            IntPtr shell = GetShellWindow();

            if (shell == IntPtr.Zero)
                throw new Exception("GetShellWindow returned NULL");

            uint pid;
            GetWindowThreadProcessId(shell, out pid);

            if (pid == 0)
                Error("GetWindowThreadProcessId");

            // Explorer process
            hp = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION,
                false,
                pid);

            if (hp == IntPtr.Zero)
                Error("OpenProcess(explorer)");

            // Explorer primary token
            if (!OpenProcessToken(
                    hp,
                    TOKEN_QUERY | TOKEN_DUPLICATE,
                    out ht))
                Error("OpenProcessToken(explorer)");

            // Make our own primary token with the required rights
            uint access =
                TOKEN_ASSIGN_PRIMARY |
                TOKEN_DUPLICATE |
                TOKEN_QUERY |
                TOKEN_ADJUST_DEFAULT |
                TOKEN_ADJUST_SESSIONID;

            if (!DuplicateTokenEx(
                    ht,
                    access,
                    IntPtr.Zero,
                    2,      // SecurityImpersonation
                    1,      // TokenPrimary
                    out hd))
                Error("DuplicateTokenEx(explorer)");

            if (!CreateEnvironmentBlock(out environment, hd, false))
                Error("CreateEnvironmentBlock");

            string cmdline = "\"" + exe + "\"";

            if (!String.IsNullOrWhiteSpace(args))
                cmdline += " " + args;

            var cmd = new StringBuilder(cmdline);

            STARTUPINFO si = new STARTUPINFO();
            si.cb = Marshal.SizeOf(typeof(STARTUPINFO));

            PROCESS_INFORMATION pi;

            if (!CreateProcessWithTokenW(
                    hd,
                    0,
                    exe,
                    cmd,
                    CREATE_UNICODE_ENVIRONMENT,
                    environment,
                    System.IO.Path.GetDirectoryName(exe),
                    ref si,
                    out pi))
                Error("CreateProcessWithTokenW");

            try
            {
                if (wait)
                {
                    if (WaitForSingleObject(pi.hProcess, INFINITE) != 0)
                        Error("WaitForSingleObject");
                    uint exitCode;
                    if (!GetExitCodeProcess(pi.hProcess, out exitCode))
                        Error("GetExitCodeProcess");
                    return exitCode;
                }
                return 0;
            }
            finally
            {
                CloseHandle(pi.hThread);
                CloseHandle(pi.hProcess);
            }
        }
        finally
        {
            if (environment != IntPtr.Zero) DestroyEnvironmentBlock(environment);
            if (hd != IntPtr.Zero) CloseHandle(hd);
            if (ht != IntPtr.Zero) CloseHandle(ht);
            if (hp != IntPtr.Zero) CloseHandle(hp);
        }
    }
}
'@
    # Build-time validation only: compile the launcher but never launch a program.
    if ($ValidateOnly) { exit 0 }
    $broker = Join-Path $InstallDirectory 'weasel-broker.exe'
    if (-not (Test-Path -LiteralPath $broker -PathType Leaf)) { throw "Missing $broker" }
    # Launch with a token duplicated from Explorer so the broker runs in the
    # user's unelevated session even when the installer itself is elevated.
    $arguments = switch ($Mode) {
        'PostInstall' { '--post-install' }
        'PostUninstall' { '--post-uninstall' }
        default { $null }
    }
    $wait = $Mode -ne 'Launch'
    $stage = "$Mode unelevated broker command"
    $exitCode = [Unelevated]::Start($broker, $arguments, $wait)
    exit ([int]$exitCode)
} catch {
    [Console]::Error.WriteLine(('{0}: {1}' -f $stage, $_.Exception.ToString()))
    exit 1
}
