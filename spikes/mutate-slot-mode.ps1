# 槽位可管理模式的变异测试。
#
# 故意往源码里注入缺陷，确认测试会失败。全绿的测试可能只是没在看，
# 变异测试是唯一能区分「真的没问题」与「断言形同虚设」的办法。
#
# 每次构建前后都清 GUI 进程——中途留下的旧进程会占着调试端口，
# 之后每次验证都连到它，跑的始终是变异之前的二进制，表现是所有
# 变异一起存活，看上去像断言完全无效。

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot

$mutations = @(
    @{
        Name = '槽位目录长度不再固定（按占用数截断）'
        File = 'crates\omy-core\src\slotdir.rs'
        From = '        let mut out = Vec::with_capacity(DIRECTORY_LEN);
        for e in &self.entries {'
        To   = '        let mut out = Vec::with_capacity(DIRECTORY_LEN);
        for e in self.entries.iter().filter(|e| e.kind != SlotKind::Empty) {'
        Why  = '长度会随密码数量变化，24 字节恒定这条不变量被破坏——' +
               '密文长度就会泄露这个文件配了几个密码'
    },
    @{
        Name = '未知槽位类型被当成空槽'
        File = 'crates\omy-core\src\slotdir.rs'
        From = '            other => Self::Unknown(other),'
        To   = '            _ => Self::Empty,'
        Why  = '新版本写的槽在旧版本眼里成了空的，会被 add 覆盖掉'
    },
    @{
        Name = 'first_free 只看末尾，不复用中间空洞'
        File = 'crates\omy-core\src\slotdir.rs'
        From = '        self.entries.iter().position(|e| !e.kind.is_occupied())'
        To   = '        if self.used() >= SLOT_COUNT { None } else { Some(self.used()) }'
        Why  = '删掉中间某个密码后，那个槽位永远用不上；' +
               '更糟的是 used() 这个下标上可能还挂着人，add 会顶掉它'
    },
    @{
        Name = 'rewrite_slots_managed 不校验「至少留一个」'
        File = 'crates\omy-core\src\keyslot.rs'
        From = '    if !plans.iter().any(|p| matches!(p, SlotPlan::Keep | SlotPlan::Write(_))) {'
        To   = '    if false {'
        Why  = '能写出一个谁也打不开的文件，用户同时失去文件和访问权'
    },
    @{
        # 原先想变异 rewrite_slots_managed 开头那句 flag 检查，但它等于
        # 空操作：去掉之后 replace_tlv_value 仍会因为找不到 TLV 而报错，
        # 程序的可观察行为一点没变。变异存活时先问「它真的改变了行为吗」
        Name = 'Keep 不保留原槽位，改填随机'
        File = 'crates\omy-core\src\keyslot.rs'
        From = '            SlotPlan::Keep => {'
        To   = '            SlotPlan::Keep if false => {'
        Why  = 'Keep 形同 Clear，改一个密码会把其它密码连同恢复码全抹掉'
    },
    @{
        Name = '改写后不更新槽位目录（沿用旧的）'
        File = 'crates\omy-core\src\keyslot.rs'
        From = '        &new_directory.encode(),'
        To   = '        &crate::slotdir::SlotDirectory::new().encode(),'
        Why  = '目录与实际槽位不一致：界面显示一个空目录，' +
               '而文件明明有密码；用户据此去 add 会顶掉现有的'
    }
)

function Build {
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    # 每次都重置：上一条命令的退出码会残留，直接读会把成功判成失败
    $global:LASTEXITCODE = 0
    & cargo build -p omy-core -p omy-cli 2>&1 | Out-Null
    return ($global:LASTEXITCODE -eq 0)
}

function RunTests {
    $o = & cargo test -p omy-core --test slot_managed 2>&1 | Out-String
    $u = & cargo test -p omy-core slotdir 2>&1 | Out-String
    return ($o + $u)
}

function Failed([string]$Out) {
    # PowerShell 的 -match 默认不区分大小写，所以 '\bFAILED\b' 会命中
    # 每一行都有的 "0 failed"——用 -cmatch 强制区分大小写。
    # 不这样的话基线永远判为失败，而输出看起来全是 ok，极难看出问题在这
    return ($Out -cmatch '\bFAILED\b') -or ($Out -cmatch 'panicked at') -or
           ($Out -match 'test result: FAILED') -or
           ($Out -cmatch '(?m)^\s*\d+ failed' -and $Out -notmatch '0 failed')
}

Write-Output '=== 基线：先确认原始代码是全绿的 ==='
if (-not (Build)) { Write-Output 'FAIL  基线编译不过'; exit 1 }
$baseline = RunTests
if (Failed $baseline) {
    Write-Output 'FAIL  基线就没过，变异测试无从谈起'
    exit 1
}
Write-Output '  OK  基线全绿'
Write-Output ''

$killed = 0
$survived = @()

foreach ($m in $mutations) {
    $path = Join-Path $root $m.File
    $orig = [System.IO.File]::ReadAllText($path)
    $from = $m.From.Replace("`r`n", "`n")
    $probe = $orig.Replace("`r`n", "`n")
    if (-not $probe.Contains($from)) {
        Write-Output "  SKIP  $($m.Name) — 锚点未命中，变异没生效"
        $survived += "$($m.Name)（锚点未命中）"
        continue
    }
    $mutated = $probe.Replace($from, $m.To.Replace("`r`n", "`n"))
    if ($orig.Contains("`r`n")) { $mutated = $mutated.Replace("`n", "`r`n") }
    [System.IO.File]::WriteAllText($path, $mutated, (New-Object System.Text.UTF8Encoding $false))

    try {
        $compiled = Build
        if (-not $compiled) {
            # 编译不过也算被抓住：类型系统就是一道断言
            Write-Output "  KILLED  $($m.Name)（编译期）"
            $killed++
        } else {
            $out = RunTests
            if (Failed $out) {
                Write-Output "  KILLED  $($m.Name)"
                $killed++
            } else {
                Write-Output "  SURVIVED  $($m.Name)"
                Write-Output "            本该被抓：$($m.Why)"
                $survived += $m.Name
            }
        }
    } finally {
        # 还原后显式刷新 mtime。用保留时间戳的方式还原会让 cargo 认为
        # 二进制比源码新而直接复用，于是跑的还是那个带缺陷的二进制——
        # 表现是「明明还原了测试却仍然失败」，源码里又搜不到变异痕迹
        [System.IO.File]::WriteAllText($path, $orig, (New-Object System.Text.UTF8Encoding $false))
        $now = Get-Date
        (Get-Item $path).LastWriteTime = $now
    }
}

Write-Output ''
Write-Output '=== 还原后复跑，确认恢复全绿 ==='
if (-not (Build)) { Write-Output 'FAIL  还原后编译不过'; exit 1 }
$final = RunTests
if (Failed $final) {
    Write-Output 'FAIL  还原后仍有测试失败——源码没恢复干净'
    exit 1
}
Write-Output '  OK  已恢复全绿'

Write-Output ''
Write-Output "杀死 $killed / $($mutations.Count) 个变异"
if ($survived.Count -gt 0) {
    Write-Output '存活的变异（断言存在缺口）：'
    $survived | ForEach-Object { Write-Output "  - $_" }
    exit 1
}
exit 0
