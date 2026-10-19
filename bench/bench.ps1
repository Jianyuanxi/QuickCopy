param(
  # 测试数据目录：环境变量 QC_BENCH_DATA（下含 bench\small、bench\big），没设就用项目内 .bench\data
  [string]$Small = "$(if ($env:QC_BENCH_DATA) { $env:QC_BENCH_DATA } else { "$PSScriptRoot\..\.bench\data" })\bench\small",
  [string]$Big = "$(if ($env:QC_BENCH_DATA) { $env:QC_BENCH_DATA } else { "$PSScriptRoot\..\.bench\data" })\bench\big",
  # 真实项目目录（含 node_modules 等大量小文件），用环境变量 QC_BENCH_ONTO 指定
  [string]$Onto = $env:QC_BENCH_ONTO,
  [string]$DstRoot = "$PSScriptRoot\..\.bench",
  [int]$Repeat = 3,
  [string[]]$Tools = @('robocopy', 'qc'),
  [string[]]$Cases = @('small', 'onto', 'big'),
  [string]$QcArgs = ''
)
# 对比 robocopy /MT:64 和 qc：每次拷到新目录，核对文件数和字节数；大文件拷完 Flush 落盘再停表。
$ErrorActionPreference = 'Stop'
$qc = Resolve-Path "$PSScriptRoot\..\target\release\qc.exe"
New-Item -ItemType Directory -Force $DstRoot | Out-Null

function Measure-Tree($d) { Get-ChildItem -LiteralPath $d -Recurse -File -Force | Measure-Object Length -Sum }
function Flush-Tree($d) {
  Get-ChildItem -LiteralPath $d -Recurse -File -Force | Where-Object Length -ge 64MB | ForEach-Object {
    $fs = [IO.FileStream]::new($_.FullName, 'Open', 'ReadWrite', 'ReadWrite'); $fs.Flush($true); $fs.Close() }
}

$sources = @{ small = $Small; onto = $Onto; big = $Big }
foreach ($case in $Cases) {
  $src = $sources[$case]
  $want = Measure-Tree $src
  "== $case：$($want.Count) 个文件，{0:N0} MB ==" -f ($want.Sum / 1MB)
  $rows = @{}
  foreach ($r in 1..$Repeat) {
    # 轮流先后，减少缓存偏向
    $order = if ($r % 2) { $Tools } else { [array]::Reverse($Tools.Clone()); $Tools[($Tools.Count - 1)..0] }
    foreach ($tool in $order) {
      $d = Join-Path $DstRoot "$tool-$r"
      if (Test-Path $d) { Remove-Item -LiteralPath $d -Recurse -Force }
      New-Item -ItemType Directory $d | Out-Null
      $name = Split-Path $src -Leaf
      $t = [Diagnostics.Stopwatch]::StartNew()
      if ($tool -eq 'robocopy') {
        robocopy $src (Join-Path $d $name) /E /MT:64 /R:1 /W:1 /NFL /NDL /NJH /NJS /NP | Out-Null
        if ($LASTEXITCODE -ge 8) { throw "robocopy exit $LASTEXITCODE" }
      } else {
        $out = & $qc @($QcArgs -split ' ' | Where-Object { $_ }) $src $d
        if ($LASTEXITCODE -ne 0) { throw "qc 失败：$out" }
      }
      Flush-Tree $d
      $s = $t.Elapsed.TotalSeconds
      $m = Measure-Tree $d
      $ok = ($m.Count -eq $want.Count -and $m.Sum -eq $want.Sum)
      $rows[$tool] += @(('{0,5:N2}s' -f $s) + $(if ($ok) { '' } else { "(不符:$($m.Count)/$($m.Sum))" }))
      Remove-Item -LiteralPath $d -Recurse -Force
    }
  }
  foreach ($tool in $Tools) { '{0,-10} {1}' -f $tool, ($rows[$tool] -join '  ') }
}
