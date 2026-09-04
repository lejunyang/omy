# 验证 omy key 对树形加密目录的支持。
#
# 走真实二进制，不走单元测试：CLI 的价值在于命令行这一层能不能用，
# 参数解析、目录判定、退出码都只有真跑才验得到。

$ErrorActionPreference = 'Stop'
$root = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-treekey-test'
$exe = Join-Path $PSScriptRoot '..\target\debug\omy.exe'
$pass = 0
$fail = 0

function Check($name, $cond, $detail = '') {
    if ($cond) { $script:pass++; Write-Output "  PASS $name" }
    else { $script:fail++; Write-Output "  FAIL $name $detail" }
}

# 自证测的是最新构建：源码比二进制新就拒绝跑
$exeInfo = Get-Item $exe -ErrorAction SilentlyContinue
if (-not $exeInfo) { Write-Output 'FAIL 找不到 omy.exe，先 cargo build -p omy-cli'; exit 1 }
$newest = Get-ChildItem (Join-Path $PSScriptRoot '..\crates') -Recurse -Filter *.rs |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newest.LastWriteTime -gt $exeInfo.LastWriteTime) {
    Write-Output "FAIL 源码($($newest.Name))比二进制新，先 cargo build -p omy-cli"
    exit 1
}

if (Test-Path $root) { Remove-Item $root -Recurse -Force }
New-Item -ItemType Directory -Path (Join-Path $root 'src\子目录') -Force | Out-Null
Set-Content -Path (Join-Path $root 'src\顶层.txt') -Value 'TOP-CONTENT' -NoNewline
Set-Content -Path (Join-Path $root 'src\子目录\深层.txt') -Value 'DEEP-CONTENT' -NoNewline

$old = Join-Path $root 'old.txt'
$new = Join-Path $root 'new.txt'
Set-Content -Path $old -Value 'old-password-123' -NoNewline
Set-Content -Path $new -Value 'new-password-456' -NoNewline

try {
    Write-Output '=== 1. 树形加密 ==='
    & $exe encrypt (Join-Path $root 'src') --mode tree --password-file $old `
        --output-dir $root --yes 2>&1 | Out-Null
    $encDir = Get-ChildItem $root -Directory | Where-Object { $_.Name -like '*.omy' } | Select-Object -First 1
    Check '树形加密产出密文目录' ($null -ne $encDir)
    if (-not $encDir) { throw '加密失败，后续无法进行' }
    $encPath = $encDir.FullName
    # -File 不能省：密文目录名同样以 .omy 结尾，不加会把目录也数进来
    $filesBefore = (Get-ChildItem $encPath -Recurse -File -Filter *.omy).Count
    Check '密文目录里有 2 个 .omy' ($filesBefore -eq 2) "实际 $filesBefore"

    Write-Output '=== 2. 对目录跑 key change ==='
    $out = & $exe key change $encPath --password-file $old --new-password-file $new --yes 2>&1 | Out-String
    Check 'change 退出码为 0' ($LASTEXITCODE -eq 0) "实际 $LASTEXITCODE"
    Check '输出提到改写了 2 个文件' ($out -match '改写文件\s+2') $out
    Check '输出给出新路径' ($out -match '新路径') $out
    Check '不再报「读取失败」这类含糊错误' (-not ($out -match '读取.*失败')) $out

    # 根目录名必须变了
    $after = Get-ChildItem $root -Directory | Where-Object { $_.Name -like '*.omy' } | Select-Object -First 1
    Check '根目录改名了' ($after.Name -ne $encDir.Name) "前 $($encDir.Name) 后 $($after.Name)"
    $newPath = $after.FullName

    Write-Output '=== 3. 新密码能解开整棵树 ==='
    $decDir = Join-Path $root 'dec'
    New-Item -ItemType Directory -Path $decDir -Force | Out-Null
    & $exe decrypt $newPath --password-file $new --output-dir $decDir --yes 2>&1 | Out-Null
    Check 'decrypt 退出码为 0' ($LASTEXITCODE -eq 0) "实际 $LASTEXITCODE"
    $top = Get-ChildItem $decDir -Recurse -Filter '顶层.txt' | Select-Object -First 1
    $deep = Get-ChildItem $decDir -Recurse -Filter '深层.txt' | Select-Object -First 1
    Check '还原出顶层.txt' ($null -ne $top)
    Check '还原出子目录里的深层.txt' ($null -ne $deep)
    if ($top) { Check '顶层内容正确' ((Get-Content $top.FullName -Raw) -eq 'TOP-CONTENT') }
    if ($deep) { Check '深层内容正确' ((Get-Content $deep.FullName -Raw) -eq 'DEEP-CONTENT') }
    # 目录结构也要对：只比文件数会漏掉「全被拍平到根」这种缺陷
    if ($deep) { Check '子目录层级保留' ($deep.FullName -match '子目录') $deep.FullName }

    Write-Output '=== 4. 反证：旧密码必须失效 ==='
    $dec2 = Join-Path $root 'dec-old'
    New-Item -ItemType Directory -Path $dec2 -Force | Out-Null
    $o2 = & $exe decrypt $newPath --password-file $old --output-dir $dec2 --yes 2>&1 | Out-String
    Check '旧密码解树失败' ($LASTEXITCODE -ne 0) "退出码 $LASTEXITCODE"
    $leaked = Get-ChildItem $dec2 -Recurse -File -ErrorAction SilentlyContinue
    Check '旧密码没有泄出任何明文' ($null -eq $leaked -or $leaked.Count -eq 0)

    Write-Output '=== 5. add / remove 必须明确拒绝 ==='
    # 一棵树只能有一个密码：目录名由第一个 KEK 派生，多出来的密码能打开
    # 文件却解不开目录名，decrypt 会报「文件损坏」。这一组就是防止那种
    # 半残状态被当成「支持了」
    $third = Join-Path $root 'third.txt'
    Set-Content -Path $third -Value 'third-password-789' -NoNewline
    $before5 = (Get-ChildItem $root -Directory | Where-Object { $_.Name -like '*.omy' }).Name

    $o5 = & $exe key add $newPath --password-file $new --new-password-file $third --yes 2>&1 | Out-String
    Check 'add 对目录报错' ($LASTEXITCODE -ne 0) "退出码 $LASTEXITCODE"
    Check 'add 的错误说明了原因' ($o5 -match '只能有一个密码') $o5
    Check 'add 的错误指出了替代做法' ($o5 -match 'key change') $o5

    $o5b = & $exe key remove $newPath --password-file $new --yes 2>&1 | Out-String
    Check 'remove 对目录报错' ($LASTEXITCODE -ne 0) "退出码 $LASTEXITCODE"

    # 被拒绝的操作不能有任何副作用
    $after5 = (Get-ChildItem $root -Directory | Where-Object { $_.Name -like '*.omy' }).Name
    Check '被拒绝后目录名没变' ($before5 -eq $after5) "前 $before5 后 $after5"
    $d5 = Join-Path $root 'dec-after-reject'
    New-Item -ItemType Directory -Path $d5 -Force | Out-Null
    & $exe decrypt $newPath --password-file $new --output-dir $d5 --yes 2>&1 | Out-Null
    Check '被拒绝后原密码仍可用' ($LASTEXITCODE -eq 0) "退出码 $LASTEXITCODE"
    # 反证：被拒绝的 add 不该真的加上密码
    $d5b = Join-Path $root 'dec-third-should-fail'
    New-Item -ItemType Directory -Path $d5b -Force | Out-Null
    & $exe decrypt $newPath --password-file $third --output-dir $d5b --yes 2>&1 | Out-Null
    Check '被拒绝的 add 没有真的加上密码' ($LASTEXITCODE -ne 0) "退出码 $LASTEXITCODE"

    Write-Output '=== 6. 错误处理 ==='
    $o6 = & $exe key change $encPath --password-file $old --new-password-file $new --yes 2>&1 | Out-String
    Check '对已失效的旧路径报错' ($LASTEXITCODE -ne 0) "退出码 $LASTEXITCODE"

    $plain = Join-Path $root 'plaindir'
    New-Item -ItemType Directory -Path $plain -Force | Out-Null
    $o7 = & $exe key change $plain --password-file $old --new-password-file $new --yes 2>&1 | Out-String
    Check '对普通目录报「不是树形加密目录」' ($o7 -match '不是树形加密') $o7

    $o8 = & $exe key reencrypt $newPath --password-file $new --yes 2>&1 | Out-String
    Check 'reencrypt 对目录明确拒绝' ($LASTEXITCODE -ne 0 -and $o8 -match '暂不支持目录') $o8

    $o9 = & $exe key change $newPath --password-file $new --new-password-file $new --yes 2>&1 | Out-String
    Check '新旧密码相同时报错' ($LASTEXITCODE -ne 0 -and $o9 -match '相同') $o9

    Write-Output '=== 7. --json 输出 ==='
    $o10 = & $exe --json key change $newPath --password-file $new --new-password-file $old --yes 2>&1 | Out-String
    Check 'json 模式退出码为 0' ($LASTEXITCODE -eq 0) $o10
    Check 'json 含 mode=tree' ($o10 -match '"mode"\s*:\s*"tree"') $o10
    Check 'json 含 files_changed' ($o10 -match '"files_changed"\s*:\s*2') $o10
    Check 'json 含 new_root' ($o10 -match '"new_root"') $o10
}
finally {
    if (Test-Path $root) { Remove-Item $root -Recurse -Force -ErrorAction SilentlyContinue }
}

Write-Output ''
Write-Output "通过 $pass / 失败 $fail"
if ($fail -gt 0) { exit 1 }
