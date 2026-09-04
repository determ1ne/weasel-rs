#Requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$InstallDirectory,
    [switch]$ValidateOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$stage = 'Initialize'
$windows = $desktop = $folder = $shell = $null
try {
    if ([Threading.Thread]::CurrentThread.GetApartmentState() -ne 'STA') {
        throw 'Run this script with powershell.exe -STA.'
    }
    # Never resolve System.dll from NSIS's plugin directory. Use explicit
    # framework references and a framework working directory for CodeDOM.
    $framework = [Runtime.InteropServices.RuntimeEnvironment]::GetRuntimeDirectory()
    Set-Location -LiteralPath $framework
    [Environment]::CurrentDirectory = $framework
    $stage = 'Compile desktop COM bridge'
    Add-Type -ReferencedAssemblies (Join-Path $framework 'System.dll') -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class DesktopShellBridge {
    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    delegate int QueryService(IntPtr self, ref Guid service, ref Guid iid, out IntPtr result);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    delegate int QueryView(IntPtr self, out IntPtr result);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    delegate int GetItemObject(IntPtr self, uint item, ref Guid iid, out IntPtr result);
    static Delegate Method(IntPtr obj, int slot, Type type) {
        return Marshal.GetDelegateForFunctionPointer(
            Marshal.ReadIntPtr(Marshal.ReadIntPtr(obj), slot * IntPtr.Size), type);
    }
    static void Check(int hr, IntPtr result, string stage) {
        if (hr < 0 || result == IntPtr.Zero)
            throw new COMException(stage, hr < 0 ? hr : unchecked((int)0x80004002));
    }
    public static object Background(object desktop) {
        IntPtr unknown = IntPtr.Zero, provider = IntPtr.Zero, browser = IntPtr.Zero;
        IntPtr view = IntPtr.Zero, background = IntPtr.Zero;
        try {
            unknown = Marshal.GetIUnknownForObject(desktop);
            var providerId = new Guid("6D5140C1-7436-11CE-8034-00AA006009FA");
            Check(Marshal.QueryInterface(unknown, ref providerId, out provider), provider, "IServiceProvider");
            var service = new Guid("4C96BE40-915C-11CF-99D3-00AA004AE837");
            var browserId = new Guid("000214E2-0000-0000-C000-000000000046");
            Check(((QueryService)Method(provider, 3, typeof(QueryService)))(provider, ref service, ref browserId, out browser), browser, "QueryService");
            Check(((QueryView)Method(browser, 15, typeof(QueryView)))(browser, out view), view, "QueryActiveShellView");
            var dispatchId = new Guid("00020400-0000-0000-C000-000000000046");
            Check(((GetItemObject)Method(view, 15, typeof(GetItemObject)))(view, 0, ref dispatchId, out background), background, "GetItemObject");
            return Marshal.GetObjectForIUnknown(background);
        } finally {
            foreach (IntPtr pointer in new [] { background, view, browser, provider, unknown })
                if (pointer != IntPtr.Zero) Marshal.Release(pointer);
        }
    }
}
'@
    # Build-time validation only: never contact Explorer or launch a program.
    if ($ValidateOnly) { exit 0 }
    $broker = Join-Path $InstallDirectory 'weasel-broker.exe'
    if (-not (Test-Path -LiteralPath $broker -PathType Leaf)) { throw "Missing $broker" }
    $stage = 'ShellWindows.FindWindowSW'
    $windows = [Activator]::CreateInstance([Type]::GetTypeFromCLSID([guid]'9BA05972-F6A8-11CF-A442-00A0C90A8F39'))
    $location = 0
    $root = $null
    $hwnd = 0
    $desktop = $windows.FindWindowSW([ref]$location, [ref]$root, 8, [ref]$hwnd, 1)
    if ($null -eq $desktop) { throw 'Explorer desktop is unavailable.' }
    $stage = 'Get desktop automation object'
    $folder = [DesktopShellBridge]::Background($desktop)
    $stage = 'Desktop.Application.ShellExecute'
    $shell = $folder.Application
    $shell.ShellExecute($broker, '', $InstallDirectory, 'open', 1)
    exit 0
} catch {
    [Console]::Error.WriteLine(('{0}: {1}' -f $stage, $_.Exception.ToString()))
    exit 1
} finally {
    foreach ($obj in @($shell, $folder, $desktop, $windows)) {
        if ($null -ne $obj -and [Runtime.InteropServices.Marshal]::IsComObject($obj)) {
            $null = [Runtime.InteropServices.Marshal]::ReleaseComObject($obj)
        }
    }
}
