#Requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$InstallDirectory,
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

    [DllImport("kernel32.dll")]
    static extern bool CloseHandle(IntPtr h);

    static void Error(string where)
    {
        throw new Win32Exception(
            Marshal.GetLastWin32Error(), where);
    }

    public static uint Start(string exe, string args)
    {
        IntPtr hp = IntPtr.Zero;
        IntPtr ht = IntPtr.Zero;
        IntPtr hd = IntPtr.Zero;

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
                    0,
                    IntPtr.Zero,
                    null,
                    ref si,
                    out pi))
                Error("CreateProcessWithTokenW");

            uint childPid = pi.dwProcessId;

            CloseHandle(pi.hThread);
            CloseHandle(pi.hProcess);

            return childPid;
        }
        finally
        {
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
    $stage = 'Launch unelevated broker'
    [void][Unelevated]::Start($broker, $null)
    exit 0
} catch {
    [Console]::Error.WriteLine(('{0}: {1}' -f $stage, $_.Exception.ToString()))
    exit 1
}
