#!/usr/bin/env pwsh
# 验证原子写入在进程被强制终止时的行为。
#
# 单元测试无法真正杀死进程，只能覆盖正常的 commit / abort 路径。
# 本脚本启动 crash_writer 示例并让它在写入中途调用 process::abort()，
# 模拟断电，然后检查磁盘状态。
#
# 用法：pwsh -File scripts/verify-atomic-write.ps1
# 前置：cargo build --example crash_writer

$ErrorActionPreference = 'Continue'
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

$dir = Join-Path $env:TEMP ('omy_crash_' + [guid]::NewGuid().ToString('N').Substring(0,8))
New-Item -ItemType Directory -Path $dir -Force | Out-Null

Write-Output "测试目录: $dir"
Write-Output ""

$exe = Join-Path $repo 'target\debug\examples\crash_writer.exe'
if (-not (Test-Path $exe)) {
    Write-Output "FAIL: 未找到 $exe"
    Write-Output "请先运行: cargo build --example crash_writer"
    exit 1
}

$pass = 0
$fail = 0
function Check($cond, $label) {
    if ($cond) { $script:pass++; Write-Output "  PASS  $label" }
    else       { $script:fail++; Write-Output "  FAIL  $label" }
}

# --- 场景 1: 进程写到一半被强杀 ---
Write-Output "=== 场景 1: 写入中途 abort 进程（模拟断电）==="
$t1 = Join-Path $dir 'crashed.omy'
$p = Start-Process -FilePath $exe -ArgumentList @($t1, 'kill') -NoNewWindow -PassThru -Wait -RedirectStandardError (Join-Path $dir 'e1.txt')
Write-Output "  进程退出码: $($p.ExitCode)（非 0 表示确实被终止）"

Check (-not (Test-Path $t1)) "目标文件不存在（崩溃未产生半成品）"
$tmps = Get-ChildItem $dir -Filter '*.tmp' -ErrorAction SilentlyContinue
Check ($tmps.Count -ge 1) "留下 .tmp 残file（可被 cleanup_stale 清理）"
if ($tmps.Count -ge 1) {
    Write-Output ("  残file: {0} ({1} 字节)" -f $tmps[0].Name, $tmps[0].Length)
}

# --- 场景 2: 正常提交 ---
Write-Output ""
Write-Output "=== 场景 2: 正常 commit ==="
$t2 = Join-Path $dir 'committed.omy'
& $exe $t2 'commit' | Out-Null
Check (Test-Path $t2) "目标文件已生成"
if (Test-Path $t2) {
    $len = (Get-Item $t2).Length
    Check ($len -eq (8 * 64 * 1024)) "文件大小正确: $len 字节（期望 524288）"
    # 内容必须全是 0xAB
    $bytes = [System.IO.File]::ReadAllBytes($t2)
    $allAB = $true
    for ($i = 0; $i -lt $bytes.Length; $i += 4096) {
        if ($bytes[$i] -ne 0xAB) { $allAB = $false; break }
    }
    Check $allAB "内容抽样校验通过（全为 0xAB）"
}
$tmpAfter = Get-ChildItem $dir -Filter 'committed*.tmp' -ErrorAction SilentlyContinue
Check ($tmpAfter.Count -eq 0) "commit 后无 .tmp 残留"

# --- 场景 3: 显式 abort ---
Write-Output ""
Write-Output "=== 场景 3: 显式 abort ==="
$t3 = Join-Path $dir 'aborted.omy'
& $exe $t3 'abort' | Out-Null
Check (-not (Test-Path $t3)) "abort 后目标文件不存在"
$tmpAb = Get-ChildItem $dir -Filter 'aborted*.tmp' -ErrorAction SilentlyContinue
Check ($tmpAb.Count -eq 0) "abort 后已清理自己的 .tmp"

# --- 场景 4: 覆盖已存在文件时，原文件必须保持完整直到 commit ---
Write-Output ""
Write-Output "=== 场景 4: 覆盖写入中途崩溃，原文件是否受损 ==="
$t4 = Join-Path $dir 'existing.omy'
$original = [byte[]](1..200)
[System.IO.File]::WriteAllBytes($t4, $original)
$origHash = (Get-FileHash $t4 -Algorithm SHA256).Hash

$p4 = Start-Process -FilePath $exe -ArgumentList @($t4, 'kill') -NoNewWindow -PassThru -Wait -RedirectStandardError (Join-Path $dir 'e4.txt')
Check (Test-Path $t4) "原文件仍然存在"
if (Test-Path $t4) {
    $nowHash = (Get-FileHash $t4 -Algorithm SHA256).Hash
    Check ($nowHash -eq $origHash) "原文件内容未被破坏（崩溃不影响已有数据）"
    Check ((Get-Item $t4).Length -eq 200) "原文件长度未变"
}

Write-Output ""
Write-Output ("=" * 60)
Write-Output "结果: $pass 项通过, $fail 项失败"
Write-Output ("=" * 60)

Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
if ($fail -gt 0) { exit 1 }
