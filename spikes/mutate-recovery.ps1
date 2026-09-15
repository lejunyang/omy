# 变异测试：恢复码的断言是否真能抓到缺陷。
#
# 19 项端到端全绿不代表断言有效。这里注入六个「看似合理的错误实现」，
# 确认每一个都被抓到。
#
# 判据顺序：先看 test result: FAILED / 失败 N 项，再看 error[E0xxx]。
# 反过来会把「断言抓到」误报成「编译失败」——那是方向相反的两个结论。

$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
$root = (Get-Location).Path

$recovery = 'crates\omy-core\src\recovery.rs'
$cli = 'crates\omy-cli\src\cmd\key.rs'
$backup = @{
    $recovery = [System.IO.File]::ReadAllBytes($recovery)
    $cli      = [System.IO.File]::ReadAllBytes($cli)
}

function Restore-All {
    foreach ($p in $backup.Keys) {
        [System.IO.File]::WriteAllBytes($p, $backup[$p])
        # 显式推进 mtime：保留时间戳会让 cargo 复用变异版二进制，
        # 表现是「明明还原了测试却仍失败」
        (Get-Item $p).LastWriteTime = Get-Date
    }
}

function Invoke-Mutation {
    param($Name, $File, $From, $To)

    $text = [System.IO.File]::ReadAllText($File)
    $mutated = $text.Replace($From, $To)
    if ($mutated -eq $text) {
        Write-Host "[$Name] 锚点未命中 —— 变异等于空操作"
        return $false
    }
    [System.IO.File]::WriteAllText($File, $mutated)
    (Get-Item $File).LastWriteTime = Get-Date

    $unit = cargo test -p omy-core --lib recovery 2>&1 | Out-String
    $build = cargo build -p omy-cli 2>&1 | Out-String
    $e2e = if ($build -match 'error\[E\d+\]|could not compile') {
        'BUILD-FAILED'
    } else {
        & pwsh -File (Join-Path $root 'spikes\verify-recovery.ps1') 2>&1 | Out-String
    }
    Restore-All

    $caught = @()
    if ($unit -match 'test result: FAILED') {
        foreach ($line in ($unit -split "`r?`n")) {
            if ($line -match '^test (\S+) \.\.\. FAILED') { $caught += "单测:$($Matches[1])" }
        }
    }
    if ($e2e -match '失败 (\d+) 项' -and [int]$Matches[1] -gt 0) {
        foreach ($line in ($e2e -split "`r?`n")) {
            # 必须锚定「两空格 + FAIL + 两空格」这个 Check 的固定格式。
            # 早先写成 'FAIL\s+(.+)'，结果把 cargo 的
            # "test result: FAILED. 3 passed..." 也匹配进来，于是归因张冠李戴
            # ——诊断时发现「restore 抽掉恢复码」被报成单测失败，
            # 而那个单测其实根本没失败。归因不准会让照着报告查问题的人走错方向
            if ($line -match '^\s+FAIL\s\s(.+)') { $caught += "端到端:$($Matches[1].Trim())" }
        }
    }
    if ($caught.Count -gt 0) {
        Write-Host "[$Name] 已被抓到 <- $((($caught | Select-Object -First 2) -join '; '))"
        return $true
    }
    if ($unit -match 'error\[E\d+\]' -or $e2e -eq 'BUILD-FAILED') {
        Write-Host "[$Name] 编译失败 —— 变异本身写错了"
        return $false
    }
    Write-Host "[$Name] **存活** —— 断言抓不到这个缺陷"
    return $false
}

$results = @()

# 1. 校验和恒为 0：抄错词、调换词序都不会被发现
$results += Invoke-Mutation '校验和形同虚设' $recovery `
    '    digest.first().map_or(0, |b| b >> 4)' `
    '    let _ = digest; 0'

# 2. 派生 KEK 时不混 vault_salt：跨库的恢复码会互相能开
$results += Invoke-Mutation '派生不绑 vault' $recovery `
    'let hk = hkdf::Hkdf::<Sha256>::new(Some(vault_salt), &self.entropy);' `
    'let hk = hkdf::Hkdf::<Sha256>::new(Some(b"fixed".as_slice()), &self.entropy);'

# 3. 不给修正建议：用户得把 26 个词从头核对
$results += Invoke-Mutation '不给修正建议' $recovery `
    '                suggestion: nearest(&w),' `
    '                suggestion: None,'

# 4. recovery 只保留恢复码：日常密码被顶掉，用户当场失去访问
$results += Invoke-Mutation 'recovery 顶掉主密码' $cli `
    '    let keep = vec![old_kek.duplicate(), reco_kek];' `
    '    let keep = vec![reco_kek];'

# 5. restore 抽掉恢复码：keep 里去掉它，且用清场策略。
#
# 必须两处一起改，这是诊断出来的：
#   - 只去掉 keep 里的 reco_kek → 空操作。恢复码在 slot 1，keep 只剩一个时
#     Carry 正好把它从旧槽原样带回，行为完全没变。
#   - 只改成 Discard → 也是空操作。keep 里还有恢复码，照样写回去。
# 判断变异有没有意义，要问「它真的改变了程序的可观察行为吗」。
$results += Invoke-Mutation 'restore 抽掉恢复码' $cli `
    '    let keep = vec![new_kek, reco_kek.duplicate()];
    let out = omy_core::keyslot::rewrite_slots(
        &data,
        &[reco_kek],
        &keep,
        omy_core::keyslot::OtherSlots::Carry,
    )?;' `
    '    let keep = vec![new_kek];
    let out = omy_core::keyslot::rewrite_slots(
        &data,
        &[reco_kek.duplicate()],
        &keep,
        omy_core::keyslot::OtherSlots::Discard,
    )?;'

# 6. 把恢复码写进 --json：等于把万能钥匙送进日志
$results += Invoke-Mutation 'JSON 泄露恢复码' $cli `
    '            "words": omy_core::recovery::WORD_COUNT,' `
    '            "words": phrase.clone(),'

Restore-All
Write-Host ''
Write-Host '=== 还原后重建并复跑，确认恢复全绿 ==='
cargo build -p omy-cli 2>&1 | Select-Object -Last 1
$u = cargo test -p omy-core --lib recovery 2>&1 | Out-String
($u -split "`r?`n") | Select-String -Pattern '^test result' | ForEach-Object { Write-Host "  单测 $($_.Line)" }
$e = & pwsh -File (Join-Path $root 'spikes\verify-recovery.ps1') 2>&1 | Out-String
($e -split "`r?`n") | Select-String -Pattern '共 \d+ 项' | ForEach-Object { Write-Host "  端到端 $($_.Line)" }

$survived = ($results | Where-Object { -not $_ }).Count
Write-Host ''
Write-Host "变异 $($results.Count) 个，存活 $survived 个"
if ($survived -gt 0) { exit 1 }
