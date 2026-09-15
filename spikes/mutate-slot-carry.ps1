# 变异测试：槽位搬运的断言是否真能抓到缺陷。
#
# 端到端全绿不代表断言有效。这里注入四个「看似合理的错误实现」，
# 确认每一个都会被抓到。
#
# 还原时显式刷新 mtime：保留时间戳会让 cargo 认为二进制比源码新而复用，
# 表现是「明明还原了测试却仍失败」。

$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
$root = (Get-Location).Path

$keyslot = 'crates\omy-core\src\keyslot.rs'
$cli = 'crates\omy-cli\src\cmd\key.rs'
$backup = @{
    $keyslot = [System.IO.File]::ReadAllBytes($keyslot)
    $cli     = [System.IO.File]::ReadAllBytes($cli)
}

function Restore-All {
    foreach ($p in $backup.Keys) {
        [System.IO.File]::WriteAllBytes($p, $backup[$p])
        (Get-Item $p).LastWriteTime = Get-Date
    }
}

function Invoke-Mutation {
    param($Name, $File, $From, $To)

    # 进度走 Write-Host：函数里的 Write-Output 会混进返回值，计数当场失真
    $text = [System.IO.File]::ReadAllText($File)
    $mutated = $text.Replace($From, $To)
    if ($mutated -eq $text) {
        Write-Host "[$Name] 锚点未命中 —— 变异等于空操作"
        return $false
    }
    [System.IO.File]::WriteAllText($File, $mutated)
    (Get-Item $File).LastWriteTime = Get-Date

    $unit = cargo test -p omy-core --lib keyslot 2>&1 | Out-String
    $build = cargo build -p omy-cli 2>&1 | Out-String
    $e2e = if ($build -match 'error\[E\d+\]|could not compile') {
        'BUILD-FAILED'
    } else {
        & pwsh -File (Join-Path $root 'spikes\verify-slot-carry.ps1') 2>&1 | Out-String
    }
    Restore-All

    # 先看断言再看编译：cargo 在测试失败时也打印 "error: test failed"，
    # 按 'error: ' 先判会把「断言抓到」误报成「编译失败」——那是两种
    # 完全不同的结论（踩过一次）
    $caught = @()
    if ($unit -match 'test result: FAILED') {
        foreach ($line in ($unit -split "`r?`n")) {
            if ($line -match '^test (\S+) \.\.\. FAILED') { $caught += "单测:$($Matches[1])" }
        }
    }
    if ($e2e -match '失败 (\d+) 项' -and [int]$Matches[1] -gt 0) {
        foreach ($line in ($e2e -split "`r?`n")) {
            if ($line -match 'FAIL\s+(.+)') { $caught += "端到端:$($Matches[1].Trim())" }
        }
    }
    if ($caught.Count -gt 0) {
        Write-Host "[$Name] 已被抓到 <- $((($caught | Select-Object -First 3) -join '; '))"
        return $true
    }
    if ($unit -match 'error\[E\d+\]' -or $e2e -eq 'BUILD-FAILED') {
        Write-Host "[$Name] 编译失败 —— 变异本身写错了，没测到东西"
        return $false
    }
    Write-Host "[$Name] **存活** —— 断言抓不到这个缺陷"
    return $false
}

$results = @()

# 1. 退回改动前：Carry 也填随机。这是最重要的一条，本次改动的全部意义
$results += Invoke-Mutation '搬运退化成填随机' $keyslot `
    '                match old_area.get(start..end) {
                    Some(slot) => {
                        w.bytes(slot);
                        carried = carried.saturating_add(1);
                    }' `
    '                match old_area.get(start..end) {
                    Some(_slot) => {
                        w.random(SLOT_LEN);
                        carried = carried.saturating_add(1);
                    }'

# 2. 搬运时下标错位：把后面的槽压缩到前面。这种错误最难查——
#    文件长度对、MAC 对、新密码能开，只有被搬错位的密码静默失效
$results += Invoke-Mutation '搬运下标错位' $keyslot `
    '            for i in keks.len()..SLOT_COUNT {
                let start = i.saturating_mul(SLOT_LEN);' `
    '            for i in keks.len()..SLOT_COUNT {
                let start = (i.saturating_sub(1)).saturating_mul(SLOT_LEN);'

# 3. change 误用 Discard（即策略传反）。CLI 层的接线错误
$results += Invoke-Mutation 'change 误用清场策略' $cli `
    '            Self::Remove => omy_core::keyslot::OtherSlots::Discard,
            Self::Add | Self::Change | Self::Reencrypt => omy_core::keyslot::OtherSlots::Carry,' `
    '            Self::Remove | Self::Change => omy_core::keyslot::OtherSlots::Discard,
            Self::Add | Self::Reencrypt => omy_core::keyslot::OtherSlots::Carry,'

# 4. remove 误用 Carry：清场变成假操作，用户以为作废了别人的密码
$results += Invoke-Mutation 'remove 误用搬运策略' $cli `
    '            Self::Remove => omy_core::keyslot::OtherSlots::Discard,' `
    '            Self::Remove => omy_core::keyslot::OtherSlots::Carry,'

# 5. 不报告 keep 增长可能顶掉槽：又一个静默毁恢复码的路径
$results += Invoke-Mutation '不报告可能顶掉的槽' $keyslot `
    'may_have_evicted: others == OtherSlots::Carry && keep.len() > 1,' `
    'may_have_evicted: false,'

Restore-All
Write-Host ''
Write-Host '=== 还原后重建并复跑，确认恢复全绿 ==='
cargo build -p omy-cli 2>&1 | Select-Object -Last 1
$u = cargo test -p omy-core --lib keyslot 2>&1 | Out-String
($u -split "`r?`n") | Select-String -Pattern '^test result' | ForEach-Object { Write-Host "  单测 $($_.Line)" }
$e = & pwsh -File (Join-Path $root 'spikes\verify-slot-carry.ps1') 2>&1 | Out-String
($e -split "`r?`n") | Select-String -Pattern '共 \d+ 项' | ForEach-Object { Write-Host "  端到端 $($_.Line)" }

$survived = ($results | Where-Object { -not $_ }).Count
Write-Host ''
Write-Host "变异 $($results.Count) 个，存活 $survived 个"
if ($survived -gt 0) { exit 1 }
