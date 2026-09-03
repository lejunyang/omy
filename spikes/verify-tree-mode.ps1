# 树形模式（模式 B）的端到端验证。
#
# 验的是文档 05 §3 那套：目录名加密、结构保留、内容可完整还原。
#
# 关键反证在第 3 节：树形模式**故意**泄露结构。这不是「顺便测一下」——
# 若哪天有人给它加了填充或诱饵，泄露量的断言会失败，那时应当改的是文档与
# UI 文案，而不是悄悄删掉断言。
#
# 与容器模式对照跑：同一棵源目录两种模式各加密一次，直接比对「暴露了什么」。
# 单独测树形模式的话，「目录名藏住了」这种断言很容易自我满足。

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$omy = Join-Path $root 'target\debug\omy.exe'

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

# 二进制必须比源码新，否则跑的是旧实现而断言看起来「就是过不了」
if (-not (Test-Path $omy)) {
    Write-Output "FAIL 找不到 $omy，先 cargo build -p omy-cli"
    exit 1
}
$binTime = (Get-Item $omy).LastWriteTime
$newer = Get-ChildItem (Join-Path $root 'crates') -Recurse -Filter *.rs |
    Where-Object { $_.LastWriteTime -gt $binTime }
if ($newer) {
    Write-Output "FAIL omy.exe 比源码旧（$($newer[0].Name)），先 cargo build -p omy-cli"
    exit 1
}

$work = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-tree-test'
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
New-Item -ItemType Directory $work | Out-Null

try {
    # ---- 造素材 ----
    $src = Join-Path $work '工作资料'
    New-Item -ItemType Directory (Join-Path $src '文档\草稿') | Out-Null
    New-Item -ItemType Directory (Join-Path $src '照片') | Out-Null
    New-Item -ItemType Directory (Join-Path $src '空目录') | Out-Null
    Set-Content -Path (Join-Path $src 'readme.txt') -Value 'top level file' -NoNewline
    Set-Content -Path (Join-Path $src '文档\报告.md') -Value '# 季度报告' -NoNewline
    Set-Content -Path (Join-Path $src '文档\草稿\draft.txt') -Value 'draft content here' -NoNewline
    # 二进制文件：确认不是只有文本能往返
    [System.IO.File]::WriteAllBytes(
        (Join-Path $src '照片\pic.bin'),
        [byte[]](0..255 | ForEach-Object { ($_ * 7) % 256 })
    )

    $env:OMY_PASSWORD_TEST = 'tree-pw'

    Write-Output ''
    Write-Output '[1] 树形模式加密'
    $encOut = Join-Path $work 'enc'
    New-Item -ItemType Directory $encOut | Out-Null
    $out = & $omy encrypt $src --mode tree --output-dir $encOut `
        --password-env OMY_PASSWORD_TEST --kdf-profile mobile 2>&1 | Out-String

    Check '命令成功' ($LASTEXITCODE -eq 0) "退出码 $LASTEXITCODE`n$out"
    # 泄露必须在动手之前告知，不是事后补一句
    Check '事先告知了泄露的元数据' ($out -match '多少文件') $out

    $encRoot = Get-ChildItem $encOut -Directory | Select-Object -First 1
    Check '产出了加密根目录' ($null -ne $encRoot) "enc 下没有目录"

    Write-Output ''
    Write-Output '[2] 目录名与文件名都不可见'
    $allNames = Get-ChildItem $encOut -Recurse | ForEach-Object { $_.Name }
    $joined = $allNames -join '|'
    foreach ($leak in @('工作资料', '文档', '草稿', '照片', '空目录', 'readme', '报告', 'draft', 'pic')) {
        Check "明文名「$leak」不出现在磁盘上" (-not ($joined -like "*$leak*")) $joined
    }
    # 密文名应当是全大写 base32（大小写不敏感的文件系统上才不会撞名）
    $dirNames = Get-ChildItem $encOut -Recurse -Directory | ForEach-Object { $_.Name }
    $allUpper = $true
    foreach ($n in $dirNames) {
        $stem = $n -replace '\.omy$', ''
        if ($stem -cne $stem.ToUpperInvariant()) { $allUpper = $false }
    }
    Check '密文目录名全大写（Windows/APFS 上不会折叠撞名）' $allUpper ($dirNames -join ',')

    Write-Output ''
    Write-Output '[3] 反证：结构故意可见（N6 已声明的泄露）'
    # 这一节断言的是「泄露确实存在」。它不是遗漏，是设计取舍——
    # 若哪天加了填充/诱饵使其不再成立，应当同步改文档与 UI 文案
    $encFiles = (Get-ChildItem $encRoot.FullName -Recurse -File |
        Where-Object { $_.Name -ne '.omy-name' }).Count
    $encDirs = (Get-ChildItem $encRoot.FullName -Recurse -Directory).Count
    Check '不解密也能数出文件个数（4 个）' ($encFiles -eq 4) "实际 $encFiles"
    Check '不解密也能看出目录层级（4 个子目录）' ($encDirs -eq 4) "实际 $encDirs"

    Write-Output ''
    Write-Output '[4] 与容器模式对照：容器藏住结构'
    $encOut2 = Join-Path $work 'enc-container'
    New-Item -ItemType Directory $encOut2 | Out-Null
    & $omy encrypt $src --mode container --output-dir $encOut2 `
        --password-env OMY_PASSWORD_TEST --kdf-profile mobile 2>&1 | Out-Null
    $cFiles = @(Get-ChildItem $encOut2 -Recurse -File)
    Check '容器模式产出单个文件' ($cFiles.Count -eq 1) "实际 $($cFiles.Count) 个"
    Check '容器模式下数不出原文件个数（对照树形的 4）' ($cFiles.Count -ne $encFiles) `
        "容器 $($cFiles.Count) vs 树形 $encFiles"

    Write-Output ''
    Write-Output '[5] 解开并逐字节核对'
    $decOut = Join-Path $work 'dec'
    New-Item -ItemType Directory $decOut | Out-Null
    $out = & $omy decrypt $encRoot.FullName --output-dir $decOut `
        --password-env OMY_PASSWORD_TEST 2>&1 | Out-String
    Check '解密命令成功' ($LASTEXITCODE -eq 0) "退出码 $LASTEXITCODE`n$out"

    $decRoot = Join-Path $decOut '工作资料'
    Check '根目录名还原成原名' (Test-Path $decRoot) "找不到 $decRoot"
    Check 'readme.txt 内容一致' `
        ((Get-Content (Join-Path $decRoot 'readme.txt') -Raw) -eq 'top level file') ''
    Check '文档\报告.md 内容一致（含中文名与中文内容）' `
        ((Get-Content (Join-Path $decRoot '文档\报告.md') -Raw) -eq '# 季度报告') ''
    Check '文档\草稿\draft.txt 内容一致（三层嵌套）' `
        ((Get-Content (Join-Path $decRoot '文档\草稿\draft.txt') -Raw) -eq 'draft content here') ''
    Check '空目录也还原了' (Test-Path (Join-Path $decRoot '空目录')) ''

    # 二进制逐字节比对：文本比对会被换行转换糊弄过去
    $orig = [System.IO.File]::ReadAllBytes((Join-Path $src '照片\pic.bin'))
    $back = [System.IO.File]::ReadAllBytes((Join-Path $decRoot '照片\pic.bin'))
    Check '二进制文件逐字节一致' `
        ($null -ne $back -and [System.Linq.Enumerable]::SequenceEqual($orig, $back)) `
        "原 $($orig.Length) 字节，还原 $($back.Length) 字节"

    Write-Output ''
    Write-Output '[6] 错误密码解不开'
    $env:OMY_WRONG = 'definitely-wrong'
    $decOut2 = Join-Path $work 'dec-wrong'
    New-Item -ItemType Directory $decOut2 | Out-Null
    $out = & $omy decrypt $encRoot.FullName --output-dir $decOut2 `
        --password-env OMY_WRONG 2>&1 | Out-String
    Check '错误密码必须失败' ($LASTEXITCODE -ne 0) "退出码 $LASTEXITCODE"
    $leaked = @(Get-ChildItem $decOut2 -Recurse -File -ErrorAction SilentlyContinue)
    Check '错误密码下没有任何明文落盘' ($leaked.Count -eq 0) "落盘了 $($leaked.Count) 个文件"

    Write-Output ''
    Write-Output '[7] --verify-only 不落盘'
    $out = & $omy decrypt $encRoot.FullName --verify-only `
        --password-env OMY_PASSWORD_TEST 2>&1 | Out-String
    Check 'verify-only 成功' ($LASTEXITCODE -eq 0) "退出码 $LASTEXITCODE`n$out"
    # 校验用的临时目录必须清掉——它是完整的明文副本
    $stale = @(Get-ChildItem (Split-Path $encRoot.FullName -Parent) -Directory `
        -Filter '.omy-verify-*' -ErrorAction SilentlyContinue)
    Check 'verify-only 后没有残留的明文临时目录' ($stale.Count -eq 0) `
        "残留 $($stale.Count) 个：$($stale.Name -join ',')"

    Write-Output ''
    Write-Output '[8] JSON 契约'
    $j = & $omy decrypt $encRoot.FullName --verify-only `
        --password-env OMY_PASSWORD_TEST --json 2>&1 | Out-String
    $obj = $null
    try { $obj = $j | ConvertFrom-Json } catch {}
    Check 'JSON 可解析' ($null -ne $obj) $j
    if ($null -ne $obj) {
        $f0 = $obj.files[0]
        Check 'JSON 标明 mode 为 tree' ($f0.mode -eq 'tree') "实际 $($f0.mode)"
        Check 'JSON 报出文件数' ($f0.files -eq 4) "实际 $($f0.files)"
    }

    Write-Output ''
    Write-Output "=== 合计 $pass 通过 / $fail 失败 ==="
    if ($fail -gt 0) { exit 1 }
} finally {
    Remove-Item Env:\OMY_PASSWORD_TEST -ErrorAction SilentlyContinue
    Remove-Item Env:\OMY_WRONG -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
