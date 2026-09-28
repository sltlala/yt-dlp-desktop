# Resize the app's main window to an exact client size (dev helper).
#
# Usage: powershell -ExecutionPolicy Bypass -File scripts/resize-window.ps1 -ProcessName ytdlp-desktop -Width 940 -Height 560
#
# NOTE: keep this file ASCII-only (Windows PowerShell 5.1 reads BOM-less UTF-8 as ANSI).

param(
    [Parameter(Mandatory = $true)][string]$ProcessName,
    [Parameter(Mandatory = $true)][int]$Width,
    [Parameter(Mandatory = $true)][int]$Height
)

Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class ResizeWin {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);

    public static IntPtr FindMain(uint target) {
        IntPtr best = IntPtr.Zero; long bestArea = -1;
        EnumWindows((h, l) => {
            uint pid; GetWindowThreadProcessId(h, out pid);
            if (pid != target || !IsWindowVisible(h)) return true;
            RECT r; if (!GetWindowRect(h, out r)) return true;
            long area = (long)(r.R - r.L) * (r.B - r.T);
            if (area > bestArea) { bestArea = area; best = h; }
            return true;
        }, IntPtr.Zero);
        return best;
    }

    // SetWindowPos sizes the OUTER window; add the non-client border so the
    // resulting client area matches the requested size.
    public static void ResizeToClient(IntPtr h, int cw, int ch) {
        RECT wr, cr; GetWindowRect(h, out wr); GetClientRect(h, out cr);
        int frameW = (wr.R - wr.L) - (cr.R - cr.L);
        int frameH = (wr.B - wr.T) - (cr.B - cr.T);
        SetWindowPos(h, IntPtr.Zero, wr.L, wr.T, cw + frameW, ch + frameH, 0x0004 /*SWP_NOZORDER*/);
    }
}
"@

[void][ResizeWin]::SetProcessDPIAware()

$proc = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $proc) { Write-Error "Process $ProcessName not found"; exit 1 }

$hwnd = [ResizeWin]::FindMain([uint32]$proc.Id)
if ($hwnd -eq [IntPtr]::Zero) { Write-Error "No window found"; exit 1 }

# SetWindowPos takes PHYSICAL pixels; the requested size is in CSS pixels.
$scale = [ResizeWin]::GetDpiForWindow($hwnd) / 96.0
if ($scale -le 0) { $scale = 1.0 }
[ResizeWin]::ResizeToClient($hwnd, [int]($Width * $scale), [int]($Height * $scale))

Write-Output "resized to client ${Width}x${Height} css (dpi scale $scale)"
