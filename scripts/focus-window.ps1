# Bring the app window to the foreground (dev helper), so clipboard tests run
# with a focused document. Kept ASCII-only on purpose: Windows PowerShell 5.1
# reads BOM-less UTF-8 as ANSI and would choke on Chinese comments here.
param([Parameter(Mandatory = $true)][string]$ProcessName)

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Fg {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
}
"@

$p = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue |
     Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $p) { Write-Output "no window for $ProcessName"; exit 1 }

[void][Fg]::ShowWindow($p.MainWindowHandle, 9)   # SW_RESTORE
Start-Sleep -Milliseconds 300
[void][Fg]::SetForegroundWindow($p.MainWindowHandle)
Start-Sleep -Milliseconds 500
$fg = [Fg]::GetForegroundWindow()
Write-Output ("foreground matched: " + ($fg -eq $p.MainWindowHandle))
