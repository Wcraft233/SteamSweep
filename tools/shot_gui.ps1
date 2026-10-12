$exe = "D:\work\kimi\steamsweep\target\debug\steamsweep.exe"
$tab = $env:STEAMSWEEP_TAB
$out = if ($tab) { "D:\work\kimi\steamsweep\gui_window_$tab.png" } else { "D:\work\kimi\steamsweep\gui_window.png" }
$p = Start-Process -FilePath $exe -PassThru
$wait = if ($env:STEAMSWEEP_WAIT) { [int]$env:STEAMSWEEP_WAIT } else { 45 }
Start-Sleep -Seconds $wait

$src = @"
using System;
using System.Runtime.InteropServices;
public class U32 {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int s);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int ht, bool repaint);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int ht, uint flags);
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@
Add-Type -TypeDefinition $src -ReferencedAssemblies System.Drawing,System.Windows.Forms

$p.Refresh()
$h = $p.MainWindowHandle
Write-Output "MainWindowHandle=$h"
if ($h -ne [IntPtr]::Zero) {
    (New-Object -ComObject Shell.Application).MinimizeAll()
    Start-Sleep -Milliseconds 800
    [U32]::ShowWindow($h, 9) | Out-Null
    [U32]::MoveWindow($h, 60, 40, 1180, 780, $true) | Out-Null
    [U32]::SetForegroundWindow($h) | Out-Null
    [U32]::SetWindowPos($h, [IntPtr]::Zero -bor -1, 0, 0, 0, 0, 3) | Out-Null  # HWND_TOPMOST, SWP_NOMOVE|SWP_NOSIZE
    Start-Sleep -Seconds 2
    $r = New-Object U32+RECT
    [U32]::GetWindowRect($h, [ref]$r) | Out-Null
    $w = $r.Right - $r.Left
    $ht = $r.Bottom - $r.Top
    Write-Output "rect: $w x $ht"
    # 直接整屏截图保存（高缩放下窗口矩形坐标系不一致，裁切会错位，整屏最稳）
    Add-Type -AssemblyName System.Windows.Forms, System.Drawing
    $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    Write-Output "screen: $($bounds.Width) x $($bounds.Height)"
    $bmp = New-Object System.Drawing.Bitmap($bounds.Width, $bounds.Height)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($bounds.Left, $bounds.Top, 0, 0, $bmp.Size)
    $bmp.Save($out)
    $g.Dispose(); $bmp.Dispose()
    Write-Output "saved"
} else {
    Write-Output "NO_WINDOW"
}
Stop-Process -Id $p.Id -Force
