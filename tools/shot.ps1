# Launch MultiVoice desktop and screenshot its window via PrintWindow (works even if covered).
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File toolsshot.ps1 -Exe <multivoice.exe> -Out <png> [-WaitMs 6000] [-Extra "--auto-join <ticket> --deafen"] [-KeepRunning]
# Keep this file ASCII-only: Windows PowerShell 5.1 reads BOM-less UTF-8 as ANSI.
param(
    [string]$Exe,
    [string]$Out,
    [string]$ProfileName = "ui-test",
    [int]$WaitMs = 3500,
    [switch]$KeepRunning,
    [string]$Extra = ""
)
$ExtraArgs = @()
if ($Extra) { $ExtraArgs = $Extra -split ' ' }
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class W {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindow(string c, string t);
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out RECT r, int s);
}
"@
[W]::SetProcessDPIAware() | Out-Null
$p = Start-Process -FilePath $Exe -ArgumentList (@("--profile", $ProfileName) + $ExtraArgs) -PassThru
$deadline = (Get-Date).AddSeconds(15)
while ((Get-Date) -lt $deadline) {
    Start-Sleep -Milliseconds 200
    $h = [W]::FindWindow([NullString]::Value, "MultiVoice")
    if ($h -ne [IntPtr]::Zero) { break }
}
if ($h -eq [IntPtr]::Zero) { "window not found"; Stop-Process $p -Force; exit 1 }
Start-Sleep -Milliseconds $WaitMs
[W]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 300
$r = New-Object W+RECT
# DWMWA_EXTENDED_FRAME_BOUNDS = 9 (window bounds without shadow)
if ([W]::DwmGetWindowAttribute($h, 9, [ref]$r, 16) -ne 0) { [W]::GetWindowRect($h, [ref]$r) | Out-Null }
$w = $r.R - $r.L; $hh = $r.B - $r.T
$bmp = New-Object System.Drawing.Bitmap $w, $hh
$g = [System.Drawing.Graphics]::FromImage($bmp)
# PW_RENDERFULLCONTENT = 2: let the window render itself even if covered (works for GPU windows)
$hdc = $g.GetHdc()
[W]::PrintWindow($h, $hdc, 2) | Out-Null
$g.ReleaseHdc($hdc)
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
"shot ${w}x${hh} -> $Out (PID $($p.Id))"
if (-not $KeepRunning) { Stop-Process $p -Force }
