param([string]$Src, [int[]]$Threads = @(8, 16, 32, 64, 128), [string]$DstRoot = "$PSScriptRoot\..\.bench", [string]$Extra = '')
# 只跑 qc，打印它自己报的扫描/复制耗时，用于找瓶颈。
$qc = Resolve-Path "$PSScriptRoot\..\target\release\qc.exe"
foreach ($n in $Threads) {
  $d = Join-Path $DstRoot "sweep-$n"
  if (Test-Path $d) { Remove-Item -LiteralPath $d -Recurse -Force }
  New-Item -ItemType Directory $d | Out-Null
  $out = & $qc --threads $n @($Extra -split ' ' | Where-Object { $_ }) $Src $d
  "{0,4} 线程  {1}" -f $n, $out
  Remove-Item -LiteralPath $d -Recurse -Force
}
