param([string]$Ui = 'win32', [string]$Shot = '', [int]$DelayMs = 1000)
# 用测试数据（2 万小文件 + 4 GB）跑一次界面进程，可选截图，打印日志尾部。
$s = if ($env:QC_BENCH_DATA) { $env:QC_BENCH_DATA } else { "$PSScriptRoot\..\.bench\data" }
$d = "$PSScriptRoot\..\.bench\ui-$Ui"; if (Test-Path $d) { Remove-Item $d -Recurse -Force }; New-Item -ItemType Directory $d | Out-Null
if ($Shot) {
  $cls = 'QuickCopyProgress'
  $cap = Start-Process pwsh -ArgumentList '-NoProfile','-File',"$PSScriptRoot\capture.ps1",'-Class',"`"$cls`"",'-Out',$Shot,'-DelayMs',$DelayMs,'-TimeoutMs','20000' -PassThru -WindowStyle Hidden -RedirectStandardOutput "$s\cap.txt"
  Start-Sleep 4
}
& "$PSScriptRoot\ticket.ps1" -Src "$s\bench\small","$s\bench\big" -Dest $d -Ui $Ui -Wait | Out-Null
if ($Shot) { $cap.WaitForExit(); Get-Content "$s\cap.txt" }
Get-Content "$env:LOCALAPPDATA\QuickCopy\quickcopy.log" -Tail 2
