param([string[]]$Src, [string]$Dest, [string]$Ui = 'win32', [switch]$Wait)
# 手写一张任务单直接拉起界面进程，绕过键盘钩子测界面。
$t = @{ job = @{ sources = $Src; dest = $Dest; mode = 'Copy' }; key_ms = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() }
$dir = Join-Path $env:LOCALAPPDATA 'QuickCopy\jobs'
New-Item -ItemType Directory -Force $dir | Out-Null
$f = Join-Path $dir "manual-$($t.key_ms).json"
$t | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $f -Encoding utf8NoBOM
$exe = Resolve-Path "$PSScriptRoot\..\target\release\qc-ui-$Ui.exe"
$p = Start-Process $exe -ArgumentList "`"$f`"" -PassThru
if ($Wait) { $p.WaitForExit() }
$p
