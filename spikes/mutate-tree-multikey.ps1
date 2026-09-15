# 树形多密码与恢复码的变异测试。
#
# 端到端全绿不等于断言有效——一套永远全绿的探针和一套真有效的探针
# 看起来一模一样。这里故意注入缺陷，确认断言会失败。
#
# 每个变异都对准一条真实后果，而不是随便改个字符：
#   1. 边车只用第一把钥匙包裹 → 回到改造前，第二个密码解不开树
#   2. 子目录不写边车 → 子目录单独拷走就解不开名字
#   3. 空槽填零 → 可否认性失效，能数出配了几把钥匙
#   4. remove 也用 Carry → 承诺作废其它密码却没作废（安全事故）
#   5. 边车绑定错 salt → 换个 vault 能解开别人的目录名
#   6. 解密时不跳过边车 → 还原结果里多出一个乱码文件

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$log = Join-Path $PSScriptRoot 'mutation-tree-log.txt'
Set-Content -LiteralPath $log -Value '' -Encoding UTF8

$targets = @{
    Sidecar = Join-Path $root 'crates\omy-core\src\dirsidecar.rs'
    Tree    = Join-Path $root 'crates\omy-core\src\tree.rs'
}
$backup = @{}
foreach ($k in $targets.Keys) {
    $backup[$k] = [System.IO.File]::ReadAllBytes($targets[$k])
}

function Restore-All {
    foreach ($k in $targets.Keys) {
        [System.IO.File]::WriteAllBytes($targets[$k], $backup[$k])
        # 必须刷新 mtime。保留时间戳的还原会让源码 mtime 倒退到变异之前，
        # cargo 认为二进制比源码新、直接复用——于是跑的还是那个带缺陷的
        # 二进制，表现为「明明还原了测试却仍然失败」
        [System.IO.File]::SetLastWriteTime($targets[$k], (Get-Date))
    }
}

function Invoke-Mutation {
    param([string]$Name, [string]$Target, [string]$Old, [string]$New, [string]$Expect)

    Write-Output ''
    Write-Output "=== 变异：$Name ==="
    Write-Output "    预期抓到：$Expect"
    Add-Content -LiteralPath $log -Value "=== $Name ===" -Encoding UTF8

    $path = $targets[$Target]
    $text = [System.IO.File]::ReadAllText($path)
    # 统一按 LF 匹配：文件可能是 CRLF，脚本里的字面量是 LF
    $wasCrlf = $text.Contains("`r`n")
    $norm = if ($wasCrlf) { $text.Replace("`r`n", "`n") } else { $text }
    $o = $Old.Replace("`r`n", "`n")
    $n = $New.Replace("`r`n", "`n")

    if (-not $norm.Contains($o)) {
        Write-Output '    SKIP  锚点未命中，变异没写对'
        Add-Content -LiteralPath $log -Value '  SKIP 锚点未命中' -Encoding UTF8
        return 'skip'
    }
    $norm = $norm.Replace($o, $n)
    $out = if ($wasCrlf) { $norm.Replace("`n", "`r`n") } else { $norm }
    [System.IO.File]::WriteAllText($path, $out, (New-Object System.Text.UTF8Encoding $false))
    [System.IO.File]::SetLastWriteTime($path, (Get-Date))

    $buildOut = & cargo build -p omy-cli 2>&1 | Out-String
    if ($buildOut -match 'error\[|could not compile') {
        Write-Output '    CAUGHT  编译失败（变异有语法/类型错）'
        Add-Content -LiteralPath $log -Value '  CAUGHT 编译失败' -Encoding UTF8
        Restore-All
        return 'caught'
    }

    # 先跑 core 单测：有几个变异（空槽填零、salt 绑定、remove 语义）是
    # 单测的职责，端到端根本观测不到它们——只跑端到端会把它们误报成
    # 「断言不足」，而实际上断言就在那里、只是没被执行
    $unitOut = & cargo test -p omy-core --lib 2>&1 | Out-String
    if ($unitOut -match 'test result: FAILED') {
        $names = [regex]::Matches($unitOut, '(?m)^test (\S+) \.\.\. FAILED') |
            ForEach-Object { $_.Groups[1].Value } | Select-Object -First 3
        Write-Output "    CAUGHT  单测：$($names -join '；')"
        Add-Content -LiteralPath $log -Value "  CAUGHT 单测 $($names -join '；')" -Encoding UTF8
        Restore-All
        return 'caught'
    }

    $verifyOut = & pwsh -File (Join-Path $PSScriptRoot 'verify-tree-multikey.ps1') 2>&1 | Out-String
    Restore-All

    # 判据顺序要紧：先看断言有没有报失败，再看是不是构建问题。
    # 反过来会把「断言抓到了」误报成「编译失败」
    if ($verifyOut -match '失败 ([1-9]\d*) 项' -or $verifyOut -match '(?m)^\s+FAIL\s\s') {
        $hits = [regex]::Matches($verifyOut, '(?m)^\s+FAIL\s\s(.+)$') |
            ForEach-Object { $_.Groups[1].Value.Trim() } | Select-Object -First 3
        Write-Output "    CAUGHT  $($hits -join '；')"
        Add-Content -LiteralPath $log -Value "  CAUGHT $($hits -join '；')" -Encoding UTF8
        return 'caught'
    }
    Write-Output '    SURVIVED  没被抓到——断言不足，或这个变异等于空操作'
    Add-Content -LiteralPath $log -Value '  SURVIVED' -Encoding UTF8
    return 'survived'
}

$results = @()

# 1. 边车只包第一把钥匙——正是改造前的行为
$results += Invoke-Mutation '边车只用第一把钥匙包裹' 'Sidecar' `
    "    for kek in keks {
        let wk = wrap_key(kek, vault_salt);" `
    "    for kek in keks.iter().take(1) {
        let wk = wrap_key(kek, vault_salt);" `
    '密码2 能解开整棵树'

# 2. 子目录不写边车
$results += Invoke-Mutation '子目录不写边车' 'Tree' `
    "            create_encrypted_dir(&dir_out, &enc, &mut rep)?;
            write_keys_sidecar(&dir_out, keks, &dk, vault_salt, opts.cipher)?;" `
    "            create_encrypted_dir(&dir_out, &enc, &mut rep)?;" `
    '每个密文目录都有 .omy-keys'

# 3. 空槽填零——可否认性失效
$results += Invoke-Mutation '边车空槽填零' 'Sidecar' `
    '    w.random(SIDECAR_SLOT_AREA.saturating_sub(used));' `
    '    w.bytes(&vec![0u8; SIDECAR_SLOT_AREA.saturating_sub(used)]);' `
    'core 单测 unused_slots_are_not_zero_filled'
# 4. 文件槽位也跟着 Carry——remove 彻底不生效
#
# 不能只改边车那一层：文件槽位仍按 Discard 走的话，被移除的密码连单个
# 文件都打不开，decrypt_tree 在读文件时就失败了，目录名层是否被清根本
# 观测不到——那样的变异等于空操作，不是断言不足。
$results += Invoke-Mutation 'remove 不再清场（两层都 Carry）' 'Tree' `
    '        let out = crate::keyslot::rewrite_slots(&data, unlock, keep, others)' `
    '        let out = crate::keyslot::rewrite_slots(&data, unlock, keep, crate::keyslot::OtherSlots::Carry)' `
    'core 单测 tree_remove_really_voids_the_other_passwords'
# 5. 边车不绑定 vault_salt
$results += Invoke-Mutation '边车不绑定 vault_salt' 'Sidecar' `
    "fn wrap_key(kek: &Kek, vault_salt: &[u8; 16]) -> SecretKey {
    kek.derive_dirname_key(vault_salt, INFO_SIDECAR_WRAP)
}" `
    "fn wrap_key(kek: &Kek, _vault_salt: &[u8; 16]) -> SecretKey {
    kek.derive_dirname_key(&[0u8; 16], INFO_SIDECAR_WRAP)
}" `
    'core 单测 sidecar_is_bound_to_vault_salt'

# 6. 解密时不跳过钥匙边车
$results += Invoke-Mutation '还原时把边车当用户文件' 'Tree' `
    "        if name == DIRNAME_SIDECAR || name == crate::dirsidecar::KEYS_SIDECAR {" `
    "        if name == DIRNAME_SIDECAR {" `
    '密码1 能解开整棵树且明文正确'

# 7. Carry 不再搬运旧包裹——改一次密码就把恢复码从边车里抹了
#
# 对准新补单测的 Carry 那半边。丢弃那半边无法用变异证伪：它传的是
# None，本来就没有旧数据可搬，任何「让它多搬一点」的变异都无从下手。
# 单测里那条断言仍有价值——它把丢弃语义钉成契约，防止将来有人图省事
# 让 rewrite_keys_sidecar 无条件读旧边车。
#
# 这个变异对应的真实后果：恢复码还能打开每个文件，却解不开目录名，
# 用户拿它解密整棵树得到「文件损坏」。
$results += Invoke-Mutation 'Carry 不再搬运旧包裹' 'Sidecar' `
    '                w.bytes(slot);
                written = written.saturating_add(1);' `
    '                let _ = slot;' `
    'core 单测 carrying_keeps_unknown_wraps_but_discarding_drops_them'
Write-Output ''
Write-Output '================ 汇总 ================'
$caught = ($results | Where-Object { $_ -eq 'caught' }).Count
$survived = ($results | Where-Object { $_ -eq 'survived' }).Count
$skipped = ($results | Where-Object { $_ -eq 'skip' }).Count
Write-Output "被抓到 $caught 项，存活 $survived 项，跳过 $skipped 项"

# 还原后必须复跑，确认恢复全绿
Write-Output ''
Write-Output '--- 还原后复跑 ---'
& cargo build -p omy-cli *> $null
$final = & pwsh -File (Join-Path $PSScriptRoot 'verify-tree-multikey.ps1') 2>&1 | Out-String
if ($final -match '通过 (\d+) 项，失败 0 项') {
    Write-Output "  OK  还原后全绿（$($Matches[1]) 项）"
} else {
    Write-Output '  FAIL  还原后没有恢复全绿，检查备份逻辑'
    ($final -split "`n" | Select-String 'FAIL' | Select-Object -First 5) |
        ForEach-Object { Write-Output "    $_" }
}

exit $(if ($survived -eq 0 -and $skipped -eq 0) { 0 } else { 1 })
