param(
  # 另一个盘上的测试目录（测跨盘剪切），默认放在系统临时目录（C 盘）
  [string]$Other = (Join-Path ([IO.Path]::GetTempPath()) 'qc-e2e'),
  [string]$ShotDir = "$PSScriptRoot\..\.tmp"
)
# 在真实资源管理器里跑一遍冲突、剪切、撤销。每个场景开一个窗口，跑完关掉。
# 撤销会把复制出来的文件送进回收站。跑完删掉撤销日志里本脚本留下的条目。
$ErrorActionPreference = 'Stop'
$base = Join-Path (Resolve-Path "$PSScriptRoot\..").Path '.tmp\e2e'
$ops = Join-Path $env:LOCALAPPDATA 'QuickCopy\ops.json'
$opsBackup = "$ops.e2e-backup"
if (Test-Path $ops) { Copy-Item $ops $opsBackup -Force }
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

# 还原撤销日志
if (Test-Path $opsBackup) { Move-Item $opsBackup $ops -Force } elseif (Test-Path $ops) { Remove-Item $ops }
""
if ($fails.Count) { "$($fails.Count) 项失败" } else { '全部通过' }
