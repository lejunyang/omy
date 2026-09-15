# 恢复码 GUI 的变异测试：故意注入缺陷，确认端到端断言会失败。
#
# 不做这一步的话，一套永远全绿的探针和一套真有效的探针看起来一模一样。
#
# 每个变异后都要重新构建**并重编 Rust**：前端是内嵌进二进制的
# （tauri.conf.json 的 frontendDist），只跑 bun run build 的话跑的还是
# 旧界面，所有变异会一起"存活"，看上去像断言全部无效。

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$fe = Join-Path $root 'crates\omy-gui\frontend'

$targets = @{
    Dialog   = Join-Path $fe 'src\components\RecoveryDialog.vue'
    Store    = Join-Path $fe 'src\store.js'
    Keymgmt  = Join-Path $root 'crates\omy-gui\src\keymgmt.rs'
}

# 备份原文（按字节，避免行尾被改写）
$backup = @{}
foreach ($k in $targets.Keys) {
    $backup[$k] = [System.IO.File]::ReadAllBytes($targets[$k])
}

function Restore-All {
    foreach ($k in $targets.Keys) {
        [System.IO.File]::WriteAllBytes($targets[$k], $backup[$k])
        # 必须刷新 mtime。用保留时间戳的方式还原会让源码 mtime 倒退到
        # 变异之前，构建工具认为产物比源码新、直接复用——于是跑的还是
        # 那个带缺陷的构建，表现为「明明还原了测试却仍然失败」
        $now = Get-Date
        [System.IO.File]::SetLastWriteTime($targets[$k], $now)
    }
}

function Invoke-Mutation {
    param([string]$Name, [string]$Target, [string]$Old, [string]$New, [string]$Expect)

    Write-Output ''
    Write-Output "=== 变异：$Name ==="
    Write-Output "    预期抓到：$Expect"
    # 明细同时写一份到文件：外层用管道过滤时，这些行常被 Select-String
    # 的缓冲吃掉，只剩汇总——而看不到"哪一项存活"就无从判断该补断言
    # 还是该改变异
    $log = Join-Path $PSScriptRoot 'mutation-log.txt'
    Add-Content -LiteralPath $log -Value "=== $Name ===" -Encoding UTF8

    $path = $targets[$Target]
    $text = [System.IO.File]::ReadAllText($path)
    if (-not $text.Contains($Old)) {
        Write-Output '    SKIP  锚点未命中，变异没写对'
        Add-Content -LiteralPath $log -Value '  SKIP 锚点未命中' -Encoding UTF8
        return 'skip'
    }
    $text = $text.Replace($Old, $New)
    [System.IO.File]::WriteAllText($path, $text, (New-Object System.Text.UTF8Encoding $false))
    [System.IO.File]::SetLastWriteTime($path, (Get-Date))

    # 跑前后都清进程：残留进程占着调试端口会让验证连到旧进程
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue

    Push-Location $fe
    $feOut = & osdk exec --tool node@22.23.2 -- bun run build 2>&1 | Out-String
    Pop-Location
    if ($feOut -notmatch 'built in') {
        Write-Output '    前端构建失败（变异可能有语法错）——算作被抓到'
        Restore-All
        return 'caught'
    }

    $rsOut = & cargo build --release -p omy-gui 2>&1 | Out-String
    if ($rsOut -match 'error\[|could not compile') {
        Write-Output '    Rust 编译失败——算作被抓到'
        Restore-All
        return 'caught'
    }

    $out = & pwsh -File (Join-Path $PSScriptRoot 'verify-recovery-gui.ps1') 2>&1 | Out-String

    Restore-All

    # 判据顺序要紧：先看断言有没有报失败，再看是不是构建问题。
    # 反过来会把「断言抓到了」误报成「编译失败」
    if ($out -match '失败 ([1-9]\d*) 项' -or $out -match '(?m)^\s+FAIL\s\s') {
        $hits = [regex]::Matches($out, '(?m)^\s+FAIL\s\s(.+)$') |
            ForEach-Object { $_.Groups[1].Value.Trim() } | Select-Object -First 4
        Write-Output "    CAUGHT  被抓到：$($hits -join '；')"
        Add-Content -LiteralPath $log -Value "  CAUGHT $($hits -join '；')" -Encoding UTF8
        return 'caught'
    }
    Write-Output '    SURVIVED  没被抓到——断言不足，或者这个变异等于空操作'
    Add-Content -LiteralPath $log -Value '  SURVIVED' -Encoding UTF8
    return 'survived'
}

$results = @()

# 每次运行前清空日志，否则多次运行的结果会叠在一起，分不清哪次是哪次
Set-Content -LiteralPath (Join-Path $PSScriptRoot 'mutation-log.txt') -Value '' -Encoding UTF8

# 1. 生成后不显示词——用户拿不到恢复码，功能等于没做
$results += Invoke-Mutation '生成后不把词传给对话框' 'Store' `
    '    const r = await api.generateRecovery(req);' `
    '    const r = await api.generateRecovery(req); if (r) r.words = [];' `
    '界面上显示了 26 个词'

# 2. 去掉「只显示一次」的警告——用户会以为还能再看到，不去抄
$results += Invoke-Mutation '去掉「只显示这一次」的警告' 'Dialog' `
    "        <div class=""warnbox"" role=""alert"">{{ i18n.t('recovery.warn_once') }}</div>" `
    '' `
    '警告了「只显示这一次」'

# 3. 不强制勾选就能关闭——摩擦消失，用户会顺手点掉
$results += Invoke-Mutation '未勾选也能关闭' 'Dialog' `
    '            :disabled="!acknowledged"' `
    '            :disabled="false"' `
    '未勾选「已抄下」时完成按钮禁用'

# 4. 把 core 的定位信息压成通用错误——最有用的提示到不了用户眼前
$results += Invoke-Mutation '丢掉错误里的 detail' 'Store' `
    "  return detail ? msg.replace('{{detail}}', detail) : msg;" `
    '  return msg;' `
    '能指出是第几个词错了 / 能给出拼写建议'

# 5. restore 时不保留恢复码——用户下次再忘密码就真的没救了。
#    必须两处一起改：只改 keep 的话 Carry 会把恢复码从旧槽原样带回，
#    等于空操作
# keymgmt.rs 是 LF，锚点不能硬编 CRLF——写死 `r`n 会让整个变异 SKIP，
# 而 SKIP 看起来很像"这项不适用"，实际是变异根本没生效
$lf = "`n"
$results += Invoke-Mutation 'restore 抽掉恢复码' 'Keymgmt' `
    ("    let keep = vec![new_kek, reco_kek.duplicate()];$lf    let out = omy_core::keyslot::rewrite_slots($lf        &data,$lf        &[reco_kek],$lf        &keep,$lf        omy_core::keyslot::OtherSlots::Carry,$lf    )") `
    ("    let keep = vec![new_kek];$lf    let out = omy_core::keyslot::rewrite_slots($lf        &data,$lf        &[reco_kek],$lf        &keep,$lf        omy_core::keyslot::OtherSlots::Discard,$lf    )") `
    '大写与多余空白不影响识别（第二次使用同一份码会失败）'

# 6. 生成时不保留当前密码——这条命令会变成「把密码换成恢复码」。
#
# 必须连 OtherSlots::Carry 一起改掉：只把当前密码从 keep 里去掉的话，
# Carry 会把它从原来的槽位原样搬回来（它本来就在文件里），行为完全没变，
# 变异等于空操作。实测确认过这一点——第一版就是这么写的，结果 SURVIVED。
$results += Invoke-Mutation '生成时丢掉当前密码' 'Keymgmt' `
    ("    let keep = vec![current.duplicate(), reco_kek];$lf    let out = omy_core::keyslot::rewrite_slots($lf        &data,$lf        &[current],$lf        &keep,$lf        // 搬运而非清场：这个文件上可能还挂着别人的密码，$lf        // 「加一个恢复码」不该顺手把它们抹了$lf        omy_core::keyslot::OtherSlots::Carry,$lf    )") `
    ("    let keep = vec![reco_kek];$lf    let out = omy_core::keyslot::rewrite_slots($lf        &data,$lf        &[current],$lf        &keep,$lf        omy_core::keyslot::OtherSlots::Discard,$lf    )") `
    'CLI 独立复核：界面设的新密码能解开（原密码被抹掉后整条链会断）'

Write-Output ''
Write-Output '================ 汇总 ================'
$caught = ($results | Where-Object { $_ -eq 'caught' }).Count
$survived = ($results | Where-Object { $_ -eq 'survived' }).Count
$skipped = ($results | Where-Object { $_ -eq 'skip' }).Count
Write-Output "被抓到 $caught 项，存活 $survived 项，跳过 $skipped 项"

# 还原后一定要复跑一次，确认恢复全绿
Write-Output ''
Write-Output '--- 还原后复跑 ---'
Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
Push-Location $fe
& osdk exec --tool node@22.23.2 -- bun run build *> $null
Pop-Location
& cargo build --release -p omy-gui *> $null
$final = & pwsh -File (Join-Path $PSScriptRoot 'verify-recovery-gui.ps1') 2>&1 | Out-String
if ($final -match '通过 (\d+) 项，失败 0 项') {
    Write-Output "  OK  还原后全绿（$($Matches[1]) 项）"
} else {
    Write-Output '  FAIL  还原后没有恢复全绿，检查备份逻辑'
    Write-Output ($final -split "`n" | Select-String 'FAIL|失败' | Select-Object -First 5)
}

exit $(if ($survived -eq 0 -and $skipped -eq 0) { 0 } else { 1 })
