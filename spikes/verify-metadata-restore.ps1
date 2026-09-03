# 端到端验证：加密文件夹再解开，元数据是否真的还原了。
#
# 为什么单独一个脚本：verify-folder-encrypt.ps1 验的是「内容能原样还原」，
# 这个验「内容之外的属性也能还原」。两者失败时要查的地方完全不同——
# 前者指向容器格式或区间计算，后者指向 restore 模块与调用时机。
#
# 关键：断言必须看**磁盘上的真实值**，不能只看命令输出说"已还原 N 项"。
# 计数是产品自己报的，用它验证产品等于没验证。

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$cli = Join-Path $root 'target\debug\omy.exe'

# ---- 新鲜度守卫：拒绝拿旧二进制测新代码 ----
# 范围只收 CLI 真正依赖的 crate。扫整个 crates 会让改了 omy-gui 也拦住
# 这个纯 CLI 脚本，那种误报会训练人无脑 rebuild，久了就把守卫当噪音。
if (-not (Test-Path $cli)) {
    Write-Output "FAIL 找不到 $cli，先 cargo build -p omy-cli"
    exit 1
}
$binTime = (Get-Item $cli).LastWriteTime
foreach ($c in 'omy-cli', 'omy-core') {
    $newer = Get-ChildItem (Join-Path $root "crates\$c\src") -Recurse -Filter *.rs |
        Where-Object { $_.LastWriteTime -gt $binTime }
    if ($newer) {
        Write-Output "FAIL 二进制比源码旧（$($newer[0].Name)），先 cargo build -p omy-cli"
        exit 1
    }
}

$pass = 0
$fail = 0
function Check($name, $cond, $detail) {
    if ($cond) {
        $script:pass++
        Write-Output "  PASS $name"
    } else {
        $script:fail++
        Write-Output "  FAIL $name  $detail"
    }
}

$work = Join-Path ([Environment]::GetFolderPath('MyDocuments')) "omy-meta-test"
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null
$env:OMY_PW = 'meta-test-pw'

try {
    # ================= 1. 造素材 =================
    Write-Output "`n[1] 造带明确时间戳的目录树"
    $src = Join-Path $work 'folder'
    New-Item -ItemType Directory -Force -Path (Join-Path $src 'sub\deeper') | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $src 'empty') | Out-Null
    Set-Content -LiteralPath (Join-Path $src 'a.txt') -Value 'aaa' -NoNewline
    Set-Content -LiteralPath (Join-Path $src 'sub\b.txt') -Value 'bbbb' -NoNewline
    Set-Content -LiteralPath (Join-Path $src 'sub\deeper\c.txt') -Value 'cc' -NoNewline

    # 每个条目给不同的时间：全用同一个值的话，"把所有条目都设成第一个
    # 条目的时间"这种缺陷检测不出来
    $times = @{
        'a.txt'            = Get-Date '2019-03-07 14:25:36'
        'sub\b.txt'        = Get-Date '2020-07-15 09:10:11'
        'sub\deeper\c.txt' = Get-Date '2021-11-23 22:33:44'
        'sub\deeper'       = Get-Date '2022-01-02 03:04:05'
        'sub'              = Get-Date '2023-05-06 07:08:09'
        'empty'            = Get-Date '2024-09-10 11:12:13'
    }
    # 先设深层再设浅层：设置文件时间不会动父目录（已实测），但新建会，
    # 所以造素材阶段仍要注意顺序
    foreach ($k in 'a.txt', 'sub\b.txt', 'sub\deeper\c.txt', 'sub\deeper', 'sub', 'empty') {
        (Get-Item (Join-Path $src $k)).LastWriteTime = $times[$k]
    }
    Write-Output "  6 个条目，各自不同的 mtime"

    # ================= 2. 加密 + 解密 =================
    Write-Output "`n[2] 加密再解开"
    $enc = Join-Path $work 'folder.omy'
    & $cli encrypt $src -o $enc --password-env OMY_PW --kdf-profile mobile --yes *> $null
    Check '加密产出文件' (Test-Path $enc) ''

    $out = Join-Path $work 'restored'
    $log = & $cli decrypt $enc -o $out --password-env OMY_PW --yes 2>&1 | Out-String
    $rroot = Join-Path $out 'folder'
    Check '解密产出目录' (Test-Path $rroot) ''

    # ================= 3. 逐条目核对磁盘上的真实时间 =================
    Write-Output "`n[3] 逐条目核对磁盘上的 mtime（不看命令输出的计数）"
    foreach ($k in $times.Keys | Sort-Object) {
        $p = Join-Path $rroot $k
        if (-not (Test-Path $p)) {
            Check "$k 存在" $false '条目未还原'
            continue
        }
        $got = (Get-Item $p).LastWriteTime
        $want = $times[$k]
        $d = [Math]::Abs(($got - $want).TotalSeconds)
        Check "$k 的 mtime" ($d -lt 2) "期望 $want，实际 $got"
    }

    # ================= 4. 内容没被元数据还原破坏 =================
    # 设置时间要拿写句柄，句柄用错会截断文件。内容校验必须一起做，
    # 否则「时间对了但文件空了」会被判成通过
    Write-Output "`n[4] 内容仍然完整"
    $expect = @{ 'a.txt' = 'aaa'; 'sub\b.txt' = 'bbbb'; 'sub\deeper\c.txt' = 'cc' }
    foreach ($k in $expect.Keys | Sort-Object) {
        $got = Get-Content -LiteralPath (Join-Path $rroot $k) -Raw
        Check "$k 内容" ($got -eq $expect[$k]) "期望 '$($expect[$k])'，实际 '$got'"
    }

    # ================= 5. 人类可读输出 =================
    Write-Output "`n[5] 报告文案"
    # Windows 上 POSIX 权限位不存在，但 pack 阶段在 Windows 上也不采集
    # mode，所以这里不该出现权限位的提示——报一个根本没保存的项是错的
    Check '不虚报权限位' ($log -notmatch 'POSIX') "输出里出现了 POSIX 提示：$log"
    Check '输出无乱码' ($log -notmatch [char]0xFFFD) ''

    # 标题以冒号结尾，就必须真的列举。曾经列举用的是 out.detail()，
    # 那是 -v 才显示的级别，于是默认输出只有「部分元数据未能还原：」
    # 加一句结语，具体是哪一项一个字都没说——而这正是用户唯一想知道的。
    # 这类缺陷不会让任何断言变红（磁盘值和 JSON 都对），只有看输出才发现
    if ($log -match '未能还原') {
        Check '报告列出了具体项' ($log -match '创建时间') `
            "只有标题没有列举，具体项可能被藏进了 -v：$log"
        Check '报告带了"值仍在文件里"的说明' ($log -match '仍保存在加密文件中') `
            '少了这句，用户会以为元数据已经丢了'
    }

    # ================= 6. JSON 契约 =================
    Write-Output "`n[6] --json 契约"
    $out2 = Join-Path $work 'restored2'
    # 结构是 { "files": [ {...} ] }——每个输入一条，metadata 在条目里。
    # 早先按顶层取，拿到 $null 就把「没有 metadata」报成产品缺陷，
    # 而磁盘上时间戳全对，两个结论矛盾时先怀疑脚本
    $raw = & $cli decrypt $enc -o $out2 --password-env OMY_PW --yes --json 2>&1 | ConvertFrom-Json
    $j = $raw.files[0]
    Check 'JSON 有 metadata 段' ($null -ne $j.metadata) "实际顶层字段：$($raw.PSObject.Properties.Name -join ',')"
    Check 'mtime_restored 为 6' ($j.metadata.mtime_restored -eq 6) "实际 $($j.metadata.mtime_restored)"
    Check 'unsupported 是数组' ($j.metadata.unsupported -is [array] -or $null -eq $j.metadata.unsupported) ''
    Check 'failures 为空' ($j.metadata.failures.Count -eq 0) "实际 $($j.metadata.failures | ConvertTo-Json -Compress)"

    # Windows 上 std 没有稳定的设置创建时间的接口，所以 btime 必然进
    # 不支持列表。这条验的是「不支持的项要如实报告」——只验 mtime 的话，
    # 报告这半个需求从未被测到，而它恰恰是决策 N3 的核心
    $items = @($j.metadata.unsupported | ForEach-Object { $_.item })
    Check 'btime 报为不支持' ($items -contains 'btime') "unsupported 实际为 $($items -join ',')"
    Check 'Windows 上不报 mode' ($items -notcontains 'mode') `
        "Windows 的 pack 阶段不采集 mode，不该出现在不支持列表里"

    # 计数必须与磁盘一致，不能只是个好看的数字
    $onDisk = 0
    foreach ($k in $times.Keys) {
        $p = Join-Path $out2 "folder\$k"
        if (Test-Path $p) {
            $d = [Math]::Abs(((Get-Item $p).LastWriteTime - $times[$k]).TotalSeconds)
            if ($d -lt 2) { $onDisk++ }
        }
    }
    Check 'JSON 计数与磁盘实况一致' ($j.metadata.mtime_restored -eq $onDisk) `
        "JSON 说 $($j.metadata.mtime_restored)，磁盘上实际对了 $onDisk 个"

    # ================= 7. 单文件不受影响 =================
    # 单文件模式没有容器索引，也就没有元数据可还原。
    # 这条防的是「给单文件路径也去跑还原」导致的报错或空报告
    Write-Output "`n[7] 单文件解密不受影响"
    $one = Join-Path $work 'one.txt'
    Set-Content -LiteralPath $one -Value 'single' -NoNewline
    $enc1 = Join-Path $work 'one.omy'
    & $cli encrypt $one -o $enc1 --password-env OMY_PW --kdf-profile mobile --yes *> $null
    $j1 = & $cli decrypt $enc1 -o (Join-Path $work 'one-out.txt') --password-env OMY_PW --yes --json 2>&1 | ConvertFrom-Json
    Check '单文件解密成功' ($null -ne $j1) ''
    Check '单文件无 metadata 段' ($null -eq $j1.metadata) "不该有：$($j1.metadata | ConvertTo-Json -Compress)"
    Check '单文件内容正确' ((Get-Content (Join-Path $work 'one-out.txt') -Raw) -eq 'single') ''

    # ================= 8. 空目录的时间也要还原 =================
    # 空目录是容器格式里显式记录的（文档 §2.2），它没有子项，
    # 所以能区分「目录时间是自己设的」还是「被子项写入带出来的」
    Write-Output "`n[8] 空目录"
    $ep = Join-Path $rroot 'empty'
    Check '空目录已还原' (Test-Path $ep) ''
    if (Test-Path $ep) {
        $d = [Math]::Abs(((Get-Item $ep).LastWriteTime - $times['empty']).TotalSeconds)
        Check '空目录 mtime' ($d -lt 2) "期望 $($times['empty'])，实际 $((Get-Item $ep).LastWriteTime)"
    }

    Write-Output "`n=== 合计 $pass 通过 / $fail 失败 ==="
} finally {
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
    Remove-Item Env:\OMY_PW -EA SilentlyContinue
}

if ($fail -gt 0) { exit 1 }
