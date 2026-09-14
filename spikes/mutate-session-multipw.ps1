# 变异测试：多密码会话的断言是否真能抓到缺陷
#
# 每个变异都对应一个**真实可能写错的实现**，而不是随便改个符号。
# 变异「存活」（测试仍全绿）说明断言不足，必须补断言而不是放过。
#
# 还原时显式刷新 mtime：shutil.copy2 式的保留时间戳会让 cargo 认为
# 二进制比源码新而直接复用旧产物，表现是「明明还原了测试却仍失败」。

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot\..

$session = 'crates\omy-core\src\session.rs'
$crypto = 'crates\omy-core\src\crypto.rs'

# 按字节备份，避免任何编码转换
$backup = @{
    $session = [System.IO.File]::ReadAllBytes($session)
    $crypto  = [System.IO.File]::ReadAllBytes($crypto)
}

function Restore-All {
    foreach ($p in $backup.Keys) {
        [System.IO.File]::WriteAllBytes($p, $backup[$p])
        # 必须显式推进 mtime，否则 cargo 复用变异版二进制
        (Get-Item $p).LastWriteTime = Get-Date
    }
}

function Invoke-Mutation {
    param($Name, $File, $From, $To)

    # 进度一律走 Write-Host：函数里的 Write-Output 会混进返回值，
    # 调用方拿到的就不是一个布尔而是「字符串 + 布尔」两项，
    # 计数当场失真（踩过：6 个变异被算成 12 个）
    $text = [System.IO.File]::ReadAllText($File)
    $mutated = $text.Replace($From, $To)
    if ($mutated -eq $text) {
        Write-Host "[$Name] 锚点未命中 —— 变异等于空操作，不算存活也不算抓到"
        return $false
    }
    [System.IO.File]::WriteAllText($File, $mutated)
    (Get-Item $File).LastWriteTime = Get-Date

    $out = cargo test -p omy-core --test runtime 2>&1 | Out-String
    Restore-All

    # 先看断言，再看编译。顺序不能反：cargo 在测试失败时也会打印
    # 「error: test failed, to rerun pass ...」，按 'error: ' 先判会把
    # **断言抓到**误报成「编译失败」——那是两种完全不同的结论，前者说明
    # 断言有效，后者说明变异根本没被执行到，等于什么都没测
    $caught = @()
    foreach ($line in ($out -split "`r?`n")) {
        if ($line -match '^test (\S+) \.\.\. FAILED') { $caught += $Matches[1] }
    }
    if ($out -match 'test result: FAILED') {
        Write-Host "[$Name] 已被抓到 <- $($caught -join ', ')"
        return $true
    }
    # 真正编译不过：用 error[E0xxx] 或 'could not compile' 判，
    # 不用宽泛的 'error: '
    if ($out -match 'error\[E\d+\]|could not compile') {
        Write-Host "[$Name] 编译失败 —— 变异本身写错了，没测到任何东西"
        return $false
    }
    Write-Host "[$Name] **存活** —— 断言抓不到这个缺陷"
    return $false
}

$results = @()

# 逐个变异。注意 $results += 只该收到一个布尔——函数里任何 Write-Output
# 都会混进来把计数搞乱
# 1. 去重判据退回 label：这正是上一轮「统一 label」的做法，
#    后果是两个不同密码重名时后者覆盖前者
$results += Invoke-Mutation '去重改按 label' $session `
    '&& v.fingerprint(vault_salt) == fp' `
    '&& k.label == label'

# 2. 指纹不绑 vault：改用固定 salt 后，同一密码在任何库里指纹都一样，
#    于是第二个库被判成「已经装过了」而装不进去
#
#    注意这里变异的是**指纹的 salt**，而不是 add_password 里那个
#    `k.vault_salt == vault_salt` 检查。试过后者，它等于空操作：指纹本身
#    已经用 vault_salt 做 HKDF salt，跨库天然不同，去掉那个检查也不会
#    误判。判断一个变异有没有意义，要问「它真的改变了可观察行为吗」
$results += Invoke-Mutation '指纹不绑 vault' $crypto `
    'let k = hkdf_expand(self.0.as_bytes(), vault_salt, INFO_FINGERPRINT);' `
    'let k = hkdf_expand(self.0.as_bytes(), b"fixed-salt", INFO_FINGERPRINT);'

# 3. 永不判重：同一密码输两次算两条，状态栏计数虚高
$results += Invoke-Mutation '从不判重' $session `
    'if dup {
            return Ok(false);
        }' `
    'if false {
            return Ok(false);
        }'

# 4. 重名时覆盖而非让开：两个不同密码重名会互相挤掉
$results += Invoke-Mutation '重名直接覆盖' $session `
    'let label = self.unique_label(vault_salt, CredentialKind::Vault, label);' `
    'let label = label.to_owned();'

# 5. 指纹与目录名密钥同域。这是真实的碰撞面：dirname 密钥同样用
#    vault_salt 作 HKDF salt，**只有 info 串不同**。换成 INFO_SLOT 则是
#    空操作（slot 用 file_uuid 作 salt，本来就撞不上）——第一版变异就写
#    错在这里，白白「存活」了一轮
$results += Invoke-Mutation '指纹与目录名密钥同域' $crypto `
    'let k = hkdf_expand(self.0.as_bytes(), vault_salt, INFO_FINGERPRINT);' `
    'let k = hkdf_expand(self.0.as_bytes(), vault_salt, b"omy/v1/dirname");'

# 6. 指纹不混 KEK：所有密码指纹相同，第二个密码永远装不进去
$results += Invoke-Mutation '指纹不含 KEK' $crypto `
    'let k = hkdf_expand(self.0.as_bytes(), vault_salt, INFO_FINGERPRINT);' `
    'let k = hkdf_expand(b"fixed", vault_salt, INFO_FINGERPRINT);'

Restore-All
Write-Output ''
Write-Output '=== 还原后复跑，确认恢复全绿 ==='
$final = cargo test -p omy-core --test runtime 2>&1 | Out-String
($final -split "`r?`n") | Select-String -Pattern '^test result' | ForEach-Object { Write-Output $_.Line }

$survived = ($results | Where-Object { -not $_ }).Count
Write-Output ''
Write-Output "变异 $($results.Count) 个，存活 $survived 个"
if ($survived -gt 0) { exit 1 }
