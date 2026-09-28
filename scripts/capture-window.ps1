# Capture the full content of a process's main window.
#
# Why this exists (all three bit me at least once):
#   1. PowerShell is DPI-unaware by default, so GetWindowRect returns VIRTUALIZED
#      coordinates. On a 1440-logical-wide window at 125% scaling it reports 1454
#      instead of the real 1800, and the screenshot is silently cropped on the
#      right -- while still looking plausible, so it reads as a layout bug.
#      Fix: call SetProcessDPIAware() first.
#   2. Composited windows (WebView2) need PrintWindow with PW_RENDERFULLCONTENT (2),
#      otherwise you get a black rectangle.
#   3. SetForegroundWindow often fails silently; CopyFromScreen then grabs whatever
#      window happens to be on top instead. PrintWindow targets a specific HWND.
#
# NOTE: keep this file ASCII-only. Windows PowerShell 5.1 reads BOM-less files
# using the ANSI code page, and non-ASCII comments corrupt the parse.
#
# Usage: powershell -ExecutionPolicy Bypass -File scripts/capture-window.ps1 `
#            -ProcessName ytdlp-desktop -Out docs/screenshots/x.png

param(
    [Parameter(Mandatory = $true)][string]$ProcessName,
    [Parameter(Mandatory = $true)][string]$Out
)

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class Cap {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int L, T, R, B; }

    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);

    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);

    // MainWindowHandle is NOT reliable for Tauri/WebView2: it can point at a
    // tiny helper window (observed: 18x18) while the real window sits elsewhere.
    // Pick the largest visible top-level window instead.
    public static IntPtr FindMain(uint target) {
        IntPtr best = IntPtr.Zero;
        long bestArea = -1;
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
}
"@

[void][Cap]::SetProcessDPIAware()

$proc = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $proc) { Write-Error "Process $ProcessName not found"; exit 1 }

$hwnd = [Cap]::FindMain([uint32]$proc.Id)
if ($hwnd -eq [IntPtr]::Zero) { Write-Error "No window found for process $ProcessName"; exit 1 }

# A minimized window has no meaningful rect; restore it first.
if ([Cap]::IsIconic($hwnd)) { [void][Cap]::ShowWindow($hwnd, 9); Start-Sleep -Milliseconds 600 }

$r = New-Object Cap+RECT
[void][Cap]::GetWindowRect($hwnd, [ref]$r)
$w = $r.R - $r.L
$h = $r.B - $r.T
if ($w -le 0 -or $h -le 0) { Write-Error "Invalid window size: ${w}x${h}"; exit 1 }

$bmp = New-Object System.Drawing.Bitmap($w, $h)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
$ok = [Cap]::PrintWindow($hwnd, $hdc, 2)  # PW_RENDERFULLCONTENT
$g.ReleaseHdc($hdc)
$g.Dispose()

$dir = Split-Path $Out -Parent
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()

Write-Output "PrintWindow=$ok  window=${w}x${h}  out=$Out  $((Get-Item $Out).Length) bytes"
