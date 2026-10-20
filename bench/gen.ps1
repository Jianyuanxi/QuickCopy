param(
  # 生成到 <数据目录>\bench\small（2 万个 4–64 KB 文件）和 <数据目录>\bench\big（一个 4 GB 文件），供 bench.ps1、uirun.ps1 使用
  [string]$Dir = "$(if ($env:QC_BENCH_DATA) { $env:QC_BENCH_DATA } else { "$PSScriptRoot\..\.bench\data" })\bench\small",
  [int]$N = 20000
)
New-Item -ItemType Directory -Force $Dir | Out-Null
$rng = [Random]::new(42)
for ($i = 0; $i -lt $N; $i++) {
  $sub = Join-Path $Dir ('d{0:D3}' -f ($i % 200))
  if ($i -lt 200) { New-Item -ItemType Directory -Force $sub | Out-Null }
  $buf = New-Object byte[] ($rng.Next(4096, 65536))
  $rng.NextBytes($buf)
  [IO.File]::WriteAllBytes((Join-Path $sub "f$i.bin"), $buf)
}
# 大文件测试用的 4 GB 文件
$big = Join-Path (Split-Path $Dir) 'big'
New-Item -ItemType Directory -Force $big | Out-Null
$blk = New-Object byte[] (64MB); $rng.NextBytes($blk)
$fs = [IO.File]::Create((Join-Path $big 'big.bin'))
for ($k = 0; $k -lt 64; $k++) { $fs.Write($blk, 0, $blk.Length) }
$fs.Close()
"generated"
