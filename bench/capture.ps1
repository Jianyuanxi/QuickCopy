param([string]$Class = 'QuickCopyProgress', [string]$Out, [int]$TimeoutMs = 5000, [int]$DelayMs = 0)
# 等指定窗口类出现后用 PrintWindow 截下来存 PNG（不依赖屏幕截图，被遮挡也能截）。
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class W {
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowW(string c, string t);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint f);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
'@
[W]::SetProcessDPIAware() | Out-Null
$sw = [Diagnostics.Stopwatch]::StartNew()
do { $h = [W]::FindWindowW($Class, [NullString]::Value); if ($h -ne 0 -and [W]::IsWindowVisible($h)) { break }; Start-Sleep -Milliseconds 20 } while ($sw.ElapsedMilliseconds -lt $TimeoutMs)
if ($h -eq 0) { "没找到窗口 $Class"; exit 1 }
if ($DelayMs) { Start-Sleep -Milliseconds $DelayMs }
$r = New-Object W+RECT; [W]::GetWindowRect($h, [ref]$r) | Out-Null
$bmp = New-Object Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
$g = [Drawing.Graphics]::FromImage($bmp); $hdc = $g.GetHdc()
[W]::PrintWindow($h, $hdc, 2) | Out-Null
$g.ReleaseHdc($hdc); $g.Dispose(); $bmp.Save($Out); $bmp.Dispose()
"saved $Out ($($r.R - $r.L)x$($r.B - $r.T))"
