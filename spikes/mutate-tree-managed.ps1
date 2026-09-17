# 树形可管理模式的变异测试。
#
# 全绿的测试可能只是没在看。每个变异都对应一条「不这样会怎样」，
# 存活就说明那条断言形同虚设。

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot

$mutations = @(
    @{
        Name = '边车目录长度不再固定（按占用数截断）'
        File = 'crates\omy-core\src\dirsidecar.rs'
        From = '    let used = keks.len().saturating_mul(WRAP_LEN);
    w.random(SIDECAR_SLOT_AREA.saturating_sub(used));'
        To   = '    let used = keks.len().saturating_mul(WRAP_LEN);
    w.random(0);
    let _ = SIDECAR_SLOT_AREA.saturating_sub(used);'
        Why  = '边车长度会随钥匙数变化，直接泄露这棵树配了几把钥匙'
    },
    @{
        Name = 'open_sidecar_at 恒返回槽位 0'
        File = 'crates\omy-core\src\dirsidecar.rs'
        From = '            return Ok((DirKey::from_bytes(raw), nonce, i));'
        To   = '            return Ok((DirKey::from_bytes(raw), nonce, 0));'
        Why  = '「当前使用」会永远标在 slot 0，用户据此删除会删错人；' +
               '而 remove 的自我保护（不许删当前这把）也会失效'
    },
    @{
        Name = 'rekey 不再保住可管理模式'
        File = 'crates\omy-core\src\tree.rs'
        From = '    let blob = if was_managed {'
        To   = '    let blob = if false {'
        Why  = '换一次密码整棵树就静默退回可否认模式，用户收不到提示'
    },
    @{
        Name = '精确删除只改根目录，不递归'
        File = 'crates\omy-core\src\tree.rs'
        From = '        let Ok(rd) = std::fs::read_dir(&cur) else { continue };
        for ent in rd.flatten() {
            let sub = ent.path();
            if sub.is_dir() {
                stack.push(sub);
            }
        }
    }
    Ok(changed)'
        To   = '        let Ok(_rd) = std::fs::read_dir(&cur) else { continue };
    }
    Ok(changed)'
        Why  = '子目录的边车没改，那个子目录被单独拷走时被删的钥匙仍能解开它'
    },
    @{
        Name = '允许删掉当前正在用的钥匙'
        File = 'crates\omy-core\src\tree.rs'
        From = '    if slot == info.current {'
        To   = '    if false {'
        Why  = '用户可以把自己锁在门外，删完之后没法再操作这棵树'
    },
    @{
        # 原先想变异那句 is_managed 检查，但它等于空操作：紧接着的长度
        # 检查仍会拦住（可否认边车 408 字节，不等于可管理的 448）。
        # 变异存活时先问「它真的改变了程序的可观察行为吗」
        Name = '空槽填零而不是填随机'
        File = 'crates\omy-core\src\dirsidecar.rs'
        From = '            SidecarPlan::Clear => {
                w.random(WRAP_LEN);
            }'
        To   = '            SidecarPlan::Clear => {
                w.bytes(&[0u8; WRAP_LEN]);
            }'
        Why  = '「哪些槽在用」直接可见，边车的可否认性就没了'
    }
)

function Build {
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    $global:LASTEXITCODE = 0
    & cargo build -p omy-core -p omy-cli 2>&1 | Out-Null
    return ($global:LASTEXITCODE -eq 0)
}

function RunTests {
    $a = & cargo test -p omy-core --test tree_managed 2>&1 | Out-String
    $b = & cargo test -p omy-core dirsidecar 2>&1 | Out-String
    return ($a + $b)
}

function Failed([string]$Out) {
    # -cmatch 区分大小写：-match 会命中每行都有的「0 failed」，
    # 于是基线永远判为失败而输出看起来全是 ok
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
        # 还原后刷新 mtime：保留时间戳会让 cargo 认为二进制比源码新而
        # 直接复用，于是跑的还是带缺陷的那个
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
