param(
  # 另一个盘上的测试目录（测跨盘剪切），默认放在系统临时目录（C 盘）
  [string]$Other = (Join-Path ([IO.Path]::GetTempPath()) 'qc-e2e'),
  [string]$ShotDir = "$PSScriptRoot\..\.tmp"
)
# 在真实资源管理器里跑一遍冲突、剪切、撤销。每个场景开一个窗口，跑完关掉。
# 撤销会把复制出来的文件送进回收站。跑完删掉撤销日志里本脚本留下的条目。
$ErrorActionPreference = 'Stop'
$base = Join-Path (Resolve-Path "$PSScriptRoot\..").Path '.tmp\e2e'
# 用项目里的空数据目录跑（撤销日志、任务单、日志都在这），免得撤到用户自己的操作上。
# 不能靠把真实 ops.json 挪开：在 Claude 桌面版（MSIX）里跑时，挪走的只是包内副本，
# 读的时候会落到用户真实的撤销日志上（2026-10-22 因此撤到过用户的操作）。e2e.ps1 和它拉起的进程都继承这个变量。
$realLocal = $env:LOCALAPPDATA
$env:LOCALAPPDATA = Join-Path (Resolve-Path "$PSScriptRoot\..").Path '.tmp\e2e-appdata'
if (Test-Path $env:LOCALAPPDATA) { Remove-Item $env:LOCALAPPDATA -Recurse -Force }
New-Item -ItemType Directory $env:LOCALAPPDATA -Force | Out-Null
$fails = [Collections.Generic.List[string]]::new()

function Fresh($d) {
  if (Test-Path $d) { Get-ChildItem $d -Recurse -Force | ForEach-Object { $_.Attributes = 'Normal' }; Remove-Item $d -Recurse -Force }
  New-Item -ItemType Directory $d -Force | Out-Null
}
function Put($path, $text) {
  New-Item -ItemType Directory (Split-Path $path) -Force | Out-Null
  [IO.File]::WriteAllText($path, $text)
}
# 目录内容：相对路径=内容（文件夹记 <dir>），含隐藏文件
function Tree($d) {
  $m = [ordered]@{}
  if (-not (Test-Path $d)) { return '' }
  Get-ChildItem $d -Recurse -Force | Sort-Object FullName | ForEach-Object {
    $rel = $_.FullName.Substring($d.Length + 1)
    $m[$rel] = if ($_.PSIsContainer) { '<dir>' } else { [IO.File]::ReadAllText($_.FullName) }
  }
  ($m.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join '; '
}
function Check($name, $cond, $detail) {
  if ($cond) { "pass $name" } else { "FAIL $name  $detail"; $fails.Add($name) }
}
function E2E { & "$PSScriptRoot\e2e.ps1" @args | ForEach-Object { "    $_" } }

# 中途出错也要还原环境、退出测试用的常驻进程、换回安装版，否则开发版会带着测试数据目录一直接管用户的粘贴
try {
Fresh $base
Fresh $Other

# 1. 复制遇到冲突：第一个替换、第二个都保留；再 Ctrl+Z 撤销
$src = "$base\c-src"; $dst = "$base\c-dst"
Put "$src\a.txt" 'a-new'; Put "$src\b.txt" 'b-new'; Put "$src\c.txt" 'c-new'
Put "$dst\a.txt" 'a-old'; Put "$dst\b.txt" 'b-old'
(Get-Item "$dst\a.txt").LastWriteTime = (Get-Date).AddDays(-3)
$before = Tree $dst
E2E -Src "$src\a.txt|$src\b.txt|$src\c.txt" -Dest $dst -Dialog 'replace,keep' -Shot "$ShotDir\conflict-dialog.png"
$t = Tree $dst
Check '复制冲突：替换 a、都保留 b、复制 c' (($t -match 'a\.txt=a-new') -and ($t -match 'b \(2\)\.txt=b-new') -and ($t -match 'b\.txt=b-old') -and ($t -match 'c\.txt=c-new') -and ($t -match 'a\.txt\.~qc-[^=]+=a-old')) $t
E2E -Key Z -Dest $dst
$t = Tree $dst
Check '复制冲突后 Ctrl+Z：恢复原样' ($t -eq $before) "$t  期望 $before"

# 2. 全部跳过（勾“对剩下的都这样”）：一个都不覆盖
$before = Tree $dst
E2E -Src "$src\a.txt|$src\b.txt" -Dest $dst -Dialog 'skip!'
$t = Tree $dst
Check '全部跳过：目标不变' ($t -eq $before) $t

# 3. 冲突对话框点取消：整个操作不做（c.txt 和 newdir 也不复制，连空文件夹都不建）
Put "$src\newdir\n.txt" 'n'
E2E -Src "$src\a.txt|$src\c.txt|$src\newdir" -Dest $dst -Dialog 'cancel'
$t = Tree $dst
Check '取消：什么都不做' ($t -eq $before) $t

# 4. 同盘剪切：文件夹改名过去，剪贴板清空；Ctrl+Z 移回
$ms = "$base\m-src"; $md = "$base\m-dst"
Put "$ms\folder\x.txt" 'x'; Put "$ms\folder\sub\y.txt" 'y'; Put "$ms\one.txt" '1'
New-Item -ItemType Directory $md -Force | Out-Null
$srcBefore = Tree $ms
E2E -Src "$ms\folder|$ms\one.txt" -Dest $md -Cut
Check '同盘剪切：到了目标' ((Tree $md) -eq 'folder=<dir>; folder\sub=<dir>; folder\sub\y.txt=y; folder\x.txt=x; one.txt=1') (Tree $md)
Check '同盘剪切：源已移走' ((Tree $ms) -eq '') (Tree $ms)
E2E -Key Z -Dest $md
Check '同盘剪切后 Ctrl+Z：移回原处' ((Tree $ms) -eq $srcBefore -and (Tree $md) -eq '') "源 $(Tree $ms) 目标 $(Tree $md)"

# 5. 跨盘剪切（C → E）：复制、核对、删源；Ctrl+Z 移回
$xs = "$Other\x-src"; $xd = "$base\x-dst"
Put "$xs\folder\x.txt" 'x'; Put "$xs\folder\sub\y.txt" 'y'; Put "$xs\one.txt" '1'
New-Item -ItemType Directory $xd -Force | Out-Null
$srcBefore = Tree $xs
E2E -Src "$xs\folder|$xs\one.txt" -Dest $xd -Cut
Check '跨盘剪切：到了目标' ((Tree $xd) -eq 'folder=<dir>; folder\sub=<dir>; folder\sub\y.txt=y; folder\x.txt=x; one.txt=1') (Tree $xd)
Check '跨盘剪切：源已删除' ((Tree $xs) -eq '') (Tree $xs)
E2E -Key Z -Dest $xd
Check '跨盘剪切后 Ctrl+Z：移回原处' ((Tree $xs) -eq $srcBefore -and (Tree $xd) -eq '') "源 $(Tree $xs) 目标 $(Tree $xd)"

# 6. 撤销日志空了：Ctrl+Z 交还系统
$out = E2E -Key Z -Dest $xd
Check '没有可撤销的：Ctrl+Z 交给系统' (($out -join "`n") -match '交还系统：没有可撤销的操作') ($out -join ' / ')

# 7. 依赖目录：项目里有 node_modules（300 个文件）、__pycache__ 和 .git。
#    跳过依赖保留 .git；全部跳过只剩源码；全部复制都拷
$ps = "$base\p-src\proj"
Put "$ps\main.py" 'm'; Put "$ps\src\a.js" 'a'; Put "$ps\__pycache__\main.pyc" 'c'; Put "$ps\.git\HEAD" 'h'
1..300 | ForEach-Object { Put "$ps\node_modules\pkg$($_ % 10)\f$_.js" 'x' }
$pd = "$base\p-dst1"; New-Item -ItemType Directory $pd -Force | Out-Null
E2E -Src $ps -Dest $pd -Dialog 'skipdeps'
Check '依赖目录：跳过依赖、保留 .git' ((Tree $pd) -eq 'proj=<dir>; proj\.git=<dir>; proj\.git\HEAD=h; proj\main.py=m; proj\src=<dir>; proj\src\a.js=a') (Tree $pd)
$pd = "$base\p-dst3"; New-Item -ItemType Directory $pd -Force | Out-Null
E2E -Src $ps -Dest $pd -Dialog 'skipall'
Check '依赖目录：全部跳过' ((Tree $pd) -eq 'proj=<dir>; proj\main.py=m; proj\src=<dir>; proj\src\a.js=a') (Tree $pd)
$pd = "$base\p-dst2"; New-Item -ItemType Directory $pd -Force | Out-Null
E2E -Src $ps -Dest $pd -Dialog 'copyall'
$n = (Get-ChildItem $pd -Recurse -File -Force).Count
Check '依赖目录：全部复制' ($n -eq 304) "文件数 $n，期望 304"

} finally {
  # 换回安装版常驻进程（e2e.ps1 启动的是 target 里的开发版）。
  # 用 WMI 在 MSIX 容器外启动，否则它读写的是包内的 AppData 副本，看不到用户真实的撤销日志
  $env:LOCALAPPDATA = $realLocal
  & "$PSScriptRoot\..\target\release\quickcopy.exe" --quit
  $installed = Join-Path $realLocal 'QuickCopy\bin\quickcopy.exe'
  if (Test-Path $installed) {
    Start-Sleep -Milliseconds 500
    $r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = "`"$installed`"" }
    if ($r.ReturnValue -eq 0) { "已换回安装版 $installed" } else { "换回安装版失败（WMI 返回 $($r.ReturnValue)），请手动运行 $installed" }
  }
}
""
if ($fails.Count) { "$($fails.Count) 项失败" } else { '全部通过' }
