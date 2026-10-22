param(
  [string]$Src,
  [string]$Dest,
  [string]$Ui = 'win32',
  [switch]$Cut,
  # 按 Ctrl+V 前焦点放在哪：list 文件列表 / rename 重命名框 / address 地址栏 / search 搜索框
  [string]$Focus = 'list',
  # 粘贴到桌面：Win+D 显示桌面后按 Ctrl+V，结束再 Win+D 还原
  [switch]$Desktop,
  # 多标签页：窗口开到 $Dest 后 Ctrl+T 新开标签页，转到 $TabTo，应该粘贴进 $TabTo 而不是 $Dest
  [string]$TabTo = '',
  # V 粘贴 / Z 撤销（撤销不碰剪贴板）
  [ValidateSet('V', 'Z')] [string]$Key = 'V',
  # 冲突对话框依次怎么点，逗号隔开：replace / skip / keep / cancel，后面加 ! 表示勾上“对剩下的都这样”；
  # 依赖目录对话框：skipdeps（保留 .git）/ skipall / copyall / cancel
  [string]$Dialog = '',
  # 第一个冲突对话框出现时截图存到这里
  [string]$Shot = '',
  # 进度窗口出现 1.5 秒后截图存到这里，顺便报它是不是前台
  [string]$ProgShot = '',
  [int]$TimeoutS = 30,
  # 不重启常驻进程（测 Ctrl+Z 顺序时要保留它记下的系统操作）
  [switch]$KeepHook,
  # 按键前先在窗口里做一次系统操作：newfolder = Ctrl+Shift+N 新建文件夹再回车
  [string]$Pre = ''
)
# 端到端：常驻进程 + 真实资源管理器窗口。把文件放进剪贴板，开一个资源管理器窗口到 $Dest，
# 模拟按 Ctrl+V，等界面进程写完日志，打印这次的日志和目标里的文件数。
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type @'
using System; using System.Runtime.InteropServices; using System.Text;
public static class K {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, IntPtr pid);
  [DllImport("user32.dll")] public static extern bool GetGUIThreadInfo(uint tid, ref GUITHREADINFO g);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [StructLayout(LayoutKind.Sequential)] public struct GUITHREADINFO {
    public int cbSize; public int flags; public IntPtr active, focus, capture, menuOwner, moveSize, caret; public RECT rc; }
  public static string Cls(IntPtr h) { var s = new StringBuilder(64); GetClassNameW(h, s, 64); return s.ToString(); }
  public static string FocusClass(IntPtr fg) {
    var g = new GUITHREADINFO(); g.cbSize = Marshal.SizeOf(g);
    GetGUIThreadInfo(GetWindowThreadProcessId(fg, IntPtr.Zero), ref g); return Cls(g.focus); }
  public static void Fg(IntPtr h) { keybd_event(0x12, 0, 0, UIntPtr.Zero); SetForegroundWindow(h); keybd_event(0x12, 0, 2, UIntPtr.Zero); }
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int x, int y, uint d, UIntPtr e);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowExW(IntPtr p, IntPtr after, string c, string t);
  public static IntPtr Find(IntPtr parent, string cls) {
    IntPtr c = IntPtr.Zero;
    while ((c = FindWindowExW(parent, c, null, null)) != IntPtr.Zero) {
      if (Cls(c) == cls && IsWindowVisible(c)) return c;
      var r = Find(c, cls); if (r != IntPtr.Zero) return r;
    }
    return IntPtr.Zero; }
  // 点一下文件列表底部的空白处，让焦点落到文件列表上（新版资源管理器刚打开时焦点在 XAML 地址栏那边）
  public static string ClickList(IntPtr top) {
    var dv = Find(top, "SHELLDLL_DefView"); if (dv == IntPtr.Zero) return "没找到 SHELLDLL_DefView";
    RECT r; GetWindowRect(dv, out r);
    SetCursorPos((r.L + r.R) / 2, r.B - 15); mouse_event(2, 0, 0, 0, UIntPtr.Zero); mouse_event(4, 0, 0, 0, UIntPtr.Zero);
    return "ok"; }
  public static void Keys(params byte[] vks) {
    foreach (var v in vks) keybd_event(v, 0, 0, UIntPtr.Zero);
    for (int i = vks.Length - 1; i >= 0; i--) keybd_event(vks[i], 0, 2, UIntPtr.Zero); }
  public static byte Vk = 0x56;
  public static void CtrlV() {
    keybd_event(0x11, 0, 0, UIntPtr.Zero); keybd_event(Vk, 0, 0, UIntPtr.Zero);
    keybd_event(Vk, 0, 2, UIntPtr.Zero); keybd_event(0x11, 0, 2, UIntPtr.Zero); }
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowW(string c, string t);
  [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint f);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
'@
Add-Type -AssemblyName System.Drawing
[K]::SetProcessDPIAware() | Out-Null
[K]::Vk = if ($Key -eq 'Z') { 0x5A } else { 0x56 }
$dialogQueue = [Collections.Generic.Queue[string]]::new()
if ($Dialog) { $Dialog -split ',' | ForEach-Object { $dialogQueue.Enqueue($_.Trim()) } }
$shotDone = $false
# 看到冲突对话框就按队列点按钮：TDM_CLICK_VERIFICATION / TDM_CLICK_BUTTON 跨进程也能发
$progShotDone = $false
function Shot-Progress {
  if (-not $ProgShot -or $script:progShotDone) { return }
  $d = [K]::FindWindowW('QuickCopyProgress', [NullString]::Value)
  if ($d -eq [IntPtr]::Zero -or -not [K]::IsWindowVisible($d)) { return }
  Start-Sleep -Milliseconds 1500
  $r = New-Object K+RECT; [K]::GetWindowRect($d, [ref]$r) | Out-Null
  $bmp = New-Object Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
  $g = [Drawing.Graphics]::FromImage($bmp); $hdc = $g.GetHdc()
  [K]::PrintWindow($d, $hdc, 2) | Out-Null
  $g.ReleaseHdc($hdc); $g.Dispose(); $bmp.Save($ProgShot); $bmp.Dispose()
  $script:progShotDone = $true
  "进度窗口截图 $ProgShot 前台=$([K]::GetForegroundWindow() -eq $d)"
}
function Answer-Dialog {
  $d = [K]::FindWindowW('#32770', '替换或跳过文件')
  if ($d -eq [IntPtr]::Zero -or -not [K]::IsWindowVisible($d)) { $d = [K]::FindWindowW('#32770', '复制文件夹') }
  if ($d -eq [IntPtr]::Zero -or -not [K]::IsWindowVisible($d)) { return }
  Start-Sleep -Milliseconds 300
  if ($Shot -and -not $script:shotDone) {
    $r = New-Object K+RECT; [K]::GetWindowRect($d, [ref]$r) | Out-Null
    $bmp = New-Object Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
    $g = [Drawing.Graphics]::FromImage($bmp); $hdc = $g.GetHdc()
    [K]::PrintWindow($d, $hdc, 2) | Out-Null
    $g.ReleaseHdc($hdc); $g.Dispose(); $bmp.Save($Shot); $bmp.Dispose()
    $script:shotDone = $true
    "截图 $Shot"
  }
  if ($dialogQueue.Count -eq 0) { "对话框出现但没有安排怎么点"; return }
  $a = $dialogQueue.Dequeue()
  $all = $a.EndsWith('!'); $a = $a.TrimEnd('!')
  $id = @{ replace = 1001; skip = 1002; keep = 1003; skipdeps = 1011; copyall = 1012; skipall = 1013; cancel = 2 }[$a]
  if ($all) { [K]::SendMessageW($d, 0x471, [IntPtr]1, [IntPtr]0) | Out-Null }
  [K]::SendMessageW($d, 0x466, [IntPtr]$id, [IntPtr]0) | Out-Null
  "对话框：点 $a$(if ($all) { '（对剩下的都这样）' })"
  Start-Sleep -Milliseconds 300
}

$root = Resolve-Path "$PSScriptRoot\.."
$log = Join-Path $env:LOCALAPPDATA 'QuickCopy\quickcopy.log'
# 常驻进程按 -Ui 重启一次
if (-not $KeepHook) {
  & "$root\target\release\quickcopy.exe" --quit
  Start-Sleep -Milliseconds 300
  Start-Process "$root\target\release\quickcopy.exe" -ArgumentList '--ui', $Ui
  Start-Sleep -Milliseconds 500
}

if ($Key -eq 'V') {
  $files = New-Object System.Collections.Specialized.StringCollection
  # pwsh -File 传不了数组，多个源用 | 隔开
  $Src -split '\|' | ForEach-Object { [void]$files.Add((Resolve-Path $_).Path) }
  $data = New-Object System.Windows.Forms.DataObject
  $data.SetFileDropList($files)
  $effect = if ($Cut) { 2 } else { 5 }
  $data.SetData('Preferred DropEffect', (New-Object IO.MemoryStream (, [BitConverter]::GetBytes([int]$effect))))
  [System.Windows.Forms.Clipboard]::SetDataObject($data, $true)
}

if ($Desktop) {
  $Dest = [Environment]::GetFolderPath('Desktop')
  [K]::Keys(0x5B, 0x44); Start-Sleep -Milliseconds 800
  # 点右下角空白处（图标一般在左边），让焦点落到桌面图标列表上
  $sc = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea
  [K]::SetCursorPos($sc.Right - 40, $sc.Bottom - 40) | Out-Null
  [K]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero); [K]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 400
  $fgNow = [K]::GetForegroundWindow()
  "前台 $([K]::Cls($fgNow)) 焦点控件 $([K]::FocusClass($fgNow))"
  $mark = (Get-Item $log).Length
  [K]::CtrlV(); Start-Sleep -Seconds 3
  [K]::Keys(0x5B, 0x44)
  $fs = [IO.File]::Open($log, 'Open', 'Read', 'ReadWrite'); $fs.Seek($mark, 'Begin') | Out-Null
  (New-Object IO.StreamReader($fs, [Text.Encoding]::UTF8)).ReadToEnd(); $fs.Close()
  return
}
$Dest = (Resolve-Path $Dest).Path
$shell = New-Object -ComObject Shell.Application
$before = @($shell.Windows() | ForEach-Object { $_.HWND })
Start-Process explorer.exe "`"$Dest`""
$win = $null
for ($i = 0; $i -lt 50 -and -not $win; $i++) {
  Start-Sleep -Milliseconds 100
  $win = $shell.Windows() | Where-Object { $_.Document -and $_.Document.Folder.Self.Path -eq $Dest } | Select-Object -Last 1
}
if (-not $win) { throw "没找到资源管理器窗口 $Dest" }
$h = [IntPtr]$win.HWND
Start-Sleep -Milliseconds 800
[K]::Fg($h)
Start-Sleep -Milliseconds 300
if ($TabTo) {
  [K]::Keys(0x11, 0x54); Start-Sleep -Milliseconds 800        # Ctrl+T
  # 地址栏打字会被中文输入法截走，直接让新标签页（主页，LocationURL 为空）跳过去
  $tab = $shell.Windows() | Where-Object { $_.HWND -eq $win.HWND -and -not $_.LocationURL } | Select-Object -First 1
  if (-not $tab) { throw '没找到新开的标签页' }
  $tab.Navigate2((Resolve-Path $TabTo).Path)
  $Dest = (Resolve-Path $TabTo).Path
  $url = ([Uri]$Dest).AbsoluteUri
  for ($i = 0; $i -lt 50; $i++) {   # 等新标签页真正转到目标
    if ($shell.Windows() | Where-Object { $_.HWND -eq $win.HWND -and $_.LocationURL -eq $url }) { break }
    Start-Sleep -Milliseconds 100
  }
  Start-Sleep -Milliseconds 300
  "同窗口的标签页：" + (($shell.Windows() | Where-Object { $_.HWND -eq $win.HWND } | ForEach-Object { $_.LocationURL }) -join ' | ')
}
"点击文件列表 $([K]::ClickList($h))"
Start-Sleep -Milliseconds 300
switch ($Focus) {
  'rename' { [K]::Keys(0x24); Start-Sleep -Milliseconds 200; [K]::Keys(0x71) }   # Home 选第一项，F2
  'address' { [K]::Keys(0x12, 0x44) }                                              # Alt+D
  'search' { [K]::Keys(0x11, 0x45) }                                               # Ctrl+E
}
Start-Sleep -Milliseconds 500
if ($Pre -eq 'newfolder') {
  [K]::Keys(0x11, 0x10, 0x4E); Start-Sleep -Milliseconds 800; [K]::Keys(0x0D); Start-Sleep -Milliseconds 800
  "新建文件夹：$(Test-Path (Join-Path $Dest '新建文件夹'))"
}
$fgNow = [K]::GetForegroundWindow()
"前台 $([K]::Cls($fgNow)) 焦点控件 $([K]::FocusClass($fgNow))"

$mark = (Get-Item $log).Length
[K]::CtrlV()
$sw = [Diagnostics.Stopwatch]::StartNew()
$done = $false
while (-not $done -and $sw.Elapsed.TotalSeconds -lt $TimeoutS) {
  Start-Sleep -Milliseconds 200
  Answer-Dialog
  Shot-Progress
  $fs = [IO.File]::Open($log, 'Open', 'Read', 'ReadWrite'); $fs.Seek($mark, 'Begin') | Out-Null
  $new = (New-Object IO.StreamReader($fs, [Text.Encoding]::UTF8)).ReadToEnd(); $fs.Close()
  $done = $new -match '完成 |交还系统'
  if ($Focus -ne 'list' -and $sw.Elapsed.TotalSeconds -gt 3) { break }   # 放行的情况不会有日志
}
if ($Focus -ne 'list') { [K]::Keys(0x1B) }   # Esc 退出编辑
Start-Sleep -Milliseconds 300
$fs = [IO.File]::Open($log, 'Open', 'Read', 'ReadWrite'); $fs.Seek($mark, 'Begin') | Out-Null
(New-Object IO.StreamReader($fs, [Text.Encoding]::UTF8)).ReadToEnd(); $fs.Close()
# 盘根目录（如 U 盘）不数，整盘遍历太慢
if ([IO.Path]::GetPathRoot($Dest) -ne $Dest) {
  $n = (Get-ChildItem -LiteralPath $Dest -Recurse -File -Force | Measure-Object).Count
  "目标文件数 $n"
}
if ($Key -eq 'V' -and $Cut) {
  "剪贴板还有文件：$([System.Windows.Forms.Clipboard]::ContainsFileDropList())"
}
# 关掉本次新开的窗口。带多个标签页时 WM_CLOSE 一次只关一个标签页，要关到窗口消失为止
if (-not $before.Contains($win.HWND)) {
  $hw = $win.HWND
  for ($i = 0; $i -lt 10 -and ($shell.Windows() | Where-Object { $_.HWND -eq $hw }); $i++) {
    [K]::PostMessageW([IntPtr]$hw, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
    Start-Sleep -Milliseconds 300
  }
}
