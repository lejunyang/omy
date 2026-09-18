# 自证「只剩设备密钥」的拦截真的有效。
#
# 一条永远通过的断言等于没有断言。这道拦截防的是数据永久丢失，
# 必须确认它真能抓到。
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot

$mutations = @(
    @{
        Name = '拦截整个失效'
        File = 'crates\omy-core\src\keyslot.rs'
        From = '        if !surviving.is_empty()
            && surviving
                .iter()
                .all(|k| *k == crate::slotdir::SlotKind::Device)
        {'
        To   = '        if false {'
        Why  = '用户能删掉密码只留设备密钥，换电脑那天数据就没了'
    },
    @{
        Name = '判据从 all 改成 any（会误伤正常组合）'
        File = 'crates\omy-core\src\keyslot.rs'
        From = '                .all(|k| *k == crate::slotdir::SlotKind::Device)'
        To   = '                .any(|k| *k == crate::slotdir::SlotKind::Device)'
        Why  = '只要有设备密钥就拒绝，「密码 + 设备密钥」这个正常组合也挂了'
    },
    @{
        Name = '只看目录不看 plans（会漏掉被清掉的槽）'
        File = 'crates\omy-core\src\keyslot.rs'
        From = '            .filter(|i| {
                plans
                    .get(*i)
                    .is_some_and(|p| matches!(p, SlotPlan::Keep | SlotPlan::Write(_)))
            })'
        To   = '            .filter(|_| true)'
        Why  = '把即将被清掉的槽也算成存活，于是「只剩设备密钥」检测不出来'
    }
)

function Build {
    $global:LASTEXITCODE = 0
    & cargo build -p omy-core 2>&1 | Out-Null
    return ($global:LASTEXITCODE -eq 0)
}

function RunTests {
    $a = & cargo test -p omy-core --test device_key_guard 2>&1 | Out-String
    $b = & cargo test -p omy-core --test slot_managed 2>&1 | Out-String
    return ($a + $b)
}

function Failed([string]$Out) {
    # -cmatch 区分大小写：-match 会命中每行都有的「0 failed」
    return ($Out -cmatch '\bFAILED\b') -or ($Out -cmatch 'panicked at')
}

Write-Output '=== 基线 ==='
if (-not (Build)) { Write-Output 'FAIL  基线编译不过'; exit 1 }
if (Failed (RunTests)) { Write-Output 'FAIL  基线就没过'; exit 1 }
Write-Output '  OK  基线全绿'
Write-Output ''

$killed = 0
$survived = @()

foreach ($m in $mutations) {
    $path = Join-Path $root $m.File
    $orig = [System.IO.File]::ReadAllText($path)
    $probe = $orig.Replace("`r`n", "`n")
    $from = $m.From.Replace("`r`n", "`n")
    if (-not $probe.Contains($from)) {
        Write-Output "  SKIP  $($m.Name) — 锚点未命中"
        $survived += "$($m.Name)（锚点未命中）"
        continue
    }
    $mut = $probe.Replace($from, $m.To.Replace("`r`n", "`n"))
    if ($orig.Contains("`r`n")) { $mut = $mut.Replace("`n", "`r`n") }
    [System.IO.File]::WriteAllText($path, $mut, (New-Object System.Text.UTF8Encoding $false))

    try {
        if (-not (Build)) {
            Write-Output "  KILLED  $($m.Name)（编译期）"
            $killed++
        } elseif (Failed (RunTests)) {
            Write-Output "  KILLED  $($m.Name)"
            $killed++
        } else {
            Write-Output "  SURVIVED  $($m.Name)"
            Write-Output "            本该被抓：$($m.Why)"
            $survived += $m.Name
        }
    } finally {
        [System.IO.File]::WriteAllText($path, $orig, (New-Object System.Text.UTF8Encoding $false))
        # 还原后刷新 mtime：保留时间戳会让 cargo 复用带缺陷的二进制
        (Get-Item $path).LastWriteTime = Get-Date
    }
}

Write-Output ''
Write-Output '=== 还原后复跑 ==='
if (-not (Build)) { Write-Output 'FAIL  还原后编译不过'; exit 1 }
if (Failed (RunTests)) { Write-Output 'FAIL  还原后仍有失败'; exit 1 }
Write-Output '  OK  已恢复全绿'

Write-Output ''
Write-Output "杀死 $killed / $($mutations.Count) 个变异"
if ($survived.Count -gt 0) {
    Write-Output '存活（断言有缺口）：'
    $survived | ForEach-Object { Write-Output "  - $_" }
    exit 1
}
exit 0
