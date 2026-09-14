# 变异测试：多密码的端到端探针是否真能抓到「退回替换语义」
#
# 探针全绿不代表断言有效。这里把 GUI 的 add_password 改回
# unlock_password（也就是这次改动之前的行为），确认探针会变红。
# 不做这一步的话，探针可能只是在测「解锁没报错」。
#
# 每次变异前后都清 GUI 进程：残留进程占着调试端口，下一轮会连到旧进程，
# 跑的始终是变异前的二进制——表现是所有变异一起存活，看上去像断言全失效。

$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
$root = (Get-Location).Path

$target = 'crates\omy-gui\src\commands.rs'
$backup = [System.IO.File]::ReadAllBytes($target)

function Restore-Src {
    [System.IO.File]::WriteAllBytes($target, $backup)
    # 必须显式推进 mtime：保留时间戳会让 cargo 认为二进制比源码新而复用，
    # 于是「明明还原了却仍然失败」
    (Get-Item $target).LastWriteTime = Get-Date
}

function Clear-Gui {
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Start-Sleep -Milliseconds 600
}

function Invoke-Mutation {
    param($Name, $From, $To)

    $text = [System.IO.File]::ReadAllText($target)
    $mutated = $text.Replace($From, $To)
    if ($mutated -eq $text) {
        Write-Host "[$Name] 锚点未命中 —— 变异等于空操作"
        return $false
    }
    [System.IO.File]::WriteAllText($target, $mutated)
    (Get-Item $target).LastWriteTime = Get-Date

    Clear-Gui
    $build = cargo build --release -p omy-gui 2>&1 | Out-String
    if ($build -match 'error\[E\d+\]|could not compile') {
        Restore-Src
        Clear-Gui
        Write-Host "[$Name] 编译失败 —— 变异本身写错了，没测到东西"
        return $false
    }

    $out = & pwsh -File (Join-Path $root 'spikes\verify-multipw.ps1') 2>&1 | Out-String
    Restore-Src
    Clear-Gui

    if ($out -match '失败 (\d+) 项' -and [int]$Matches[1] -gt 0) {
        $failed = @()
        foreach ($line in ($out -split "`r?`n")) {
            if ($line -match 'FAIL\s+(.+)') { $failed += $Matches[1].Trim() }
        }
        Write-Host "[$Name] 已被抓到 <- $((($failed | Select-Object -First 3) -join '; '))"
        return $true
    }
    Write-Host "[$Name] **存活** —— 探针抓不到这个回退"
    return $false
}

$results = @()

# 唯一但最重要的变异：把累加改回替换。这正是改动前的行为，
# 也是任何人「顺手简化」时最可能退回去的样子
$results += Invoke-Mutation '退回替换语义' `
    'if let Ok(is_new) = s.add_password(&label, salt, &password, *params) {
                    ok = ok.saturating_add(1);
                    added |= is_new;
                }' `
    'if s.unlock_password(&label, salt, &password, *params).is_ok() {
                    ok = ok.saturating_add(1);
                    added = true;
                }'

Restore-Src
Clear-Gui
Write-Host ''
Write-Host '=== 还原后重建并复跑，确认恢复全绿 ==='
cargo build --release -p omy-gui 2>&1 | Select-Object -Last 1
$final = & pwsh -File (Join-Path $root 'spikes\verify-multipw.ps1') 2>&1 | Out-String
($final -split "`r?`n") | Select-String -Pattern '共 \d+ 项' | ForEach-Object { Write-Host $_.Line }
Clear-Gui

$survived = ($results | Where-Object { -not $_ }).Count
Write-Host ''
Write-Host "变异 $($results.Count) 个，存活 $survived 个"
if ($survived -gt 0) { exit 1 }
