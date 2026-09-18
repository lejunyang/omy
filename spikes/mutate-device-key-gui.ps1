# 自证界面探针真能抓到「后端全对但前端没接通」。
#
# 这个形态真实发生过：挂载、状态、移除都有，唯独没有任何地方用它解锁。
# 如果探针抓不到，它就只是在报告文案齐备。
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot

$mutations = @(
    @{
        Name = 'App.vue 不传 deviceKey 属性'
        File = 'crates\omy-gui\frontend\src\App.vue'
        From = '    :device-key="deviceKeyReady"'
        To   = ''
        Why  = 'Hello 按钮永远不出现，用户没有免密解锁入口'
    },
    @{
        Name = '按钮不再发 device-unlock 事件'
        File = 'crates\omy-gui\frontend\src\components\UnlockDialog.vue'
        From = '          @click="$emit(''device-unlock'')"'
        To   = '          @click="void 0"'
        Why  = '按钮在但点了没反应'
    },
    @{
        Name = '删掉「可能永久失效」警示'
        File = 'crates\omy-gui\frontend\src\components\SettingsDialog.vue'
        From = '                <div class="desc">{{ i18n.t(''devicekey.warn_can_be_lost'') }}</div>'
        To   = ''
        Why  = '用户不知道换电脑会失效，可能因此不再记密码——直接丢数据'
    }
)

function Rebuild {
    # 构建成功与否**不能**看退出码，两处都不可靠：
    #
    # - `bun run build` 在 `pwsh -File` 下返回 1（sourcemap 警告），而同一条
    #   命令在交互式 shell 里返回 0。据此判断会把每次构建都报成失败，
    #   而单独手跑每一步却都正常——这个矛盾极难识破，已经浪费过很多时间。
    # - PowerShell 会把函数里所有未捕获的输出并进返回值，于是
    #   `if (-not (Rebuild))` 变成对数组取反，恒为真。
    #
    # 所以：全程 Out-Null 吞输出、只在末尾 return 一次，判据看产物是不是
    # 真的被这次构建重写了。
    $ok = $true
    $dist = Join-Path $root 'crates\omy-gui\dist\app.js'
    $exe = Join-Path $root 'target\release\omy-gui.exe'

    Push-Location (Join-Path $root 'crates\omy-gui\frontend') | Out-Null
    & bun run build 2>&1 | Out-Null
    Pop-Location | Out-Null
    if (-not (Test-Path $dist)) { $ok = $false }

    if ($ok) {
        # 前端内嵌进二进制，只跑 bun 不够。
        # 先清进程：占用中的 exe 无法被覆盖，cargo 只报一个含糊的链接错误
        Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force
        Start-Sleep -Milliseconds 300
        Push-Location $root | Out-Null
        & cargo build --release -p omy-gui 2>&1 | Out-Null
        $rs = ($LASTEXITCODE -eq 0)
        Pop-Location | Out-Null
        if (-not $rs -or -not (Test-Path $exe)) { $ok = $false }
    }

    if ($ok) {
        # 两个产物的 mtime 都推到最新：还原源码时刷过 mtime，
        # 否则外层 verify 会因为「源码比产物新」而拒跑
        (Get-Item $dist).LastWriteTime = Get-Date
        (Get-Item $exe).LastWriteTime = Get-Date
    }
    return $ok
}

Write-Output '=== 基线 ==='
if (-not (Rebuild)) { Write-Output 'FAIL  基线构建不过'; exit 1 }
$out = & pwsh -File (Join-Path $PSScriptRoot 'verify-device-key-gui.ps1') 2>&1 | Out-String
if ($out -notmatch 'ʧ�� 0|失败 0') { Write-Output 'FAIL  基线就没过'; exit 1 }
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
        if (-not (Rebuild)) {
            Write-Output "  KILLED  $($m.Name)（构建期）"
            $killed++
        } else {
            $o = & pwsh -File (Join-Path $PSScriptRoot 'verify-device-key-gui.ps1') 2>&1 | Out-String
            if ($o -match 'ʧ�� 0|失败 0') {
                Write-Output "  SURVIVED  $($m.Name)"
                Write-Output "            本该被抓：$($m.Why)"
                $survived += $m.Name
            } else {
                Write-Output "  KILLED  $($m.Name)"
                $killed++
            }
        }
    } finally {
        [System.IO.File]::WriteAllText($path, $orig, (New-Object System.Text.UTF8Encoding $false))
        # 还原后刷新 mtime：保留时间戳会让构建复用带缺陷的产物，
        # 表现是「明明已经还原了，测试却仍然失败」
        (Get-Item $path).LastWriteTime = Get-Date
    }
}

Write-Output ''
Write-Output '=== 还原后复跑 ==='
if (-not (Rebuild)) { Write-Output 'FAIL  还原后构建不过'; exit 1 }
$out = & pwsh -File (Join-Path $PSScriptRoot 'verify-device-key-gui.ps1') 2>&1 | Out-String
if ($out -notmatch 'ʧ�� 0|失败 0') { Write-Output 'FAIL  还原后仍有失败'; exit 1 }
Write-Output '  OK  已恢复全绿'

Write-Output ''
Write-Output "杀死 $killed / $($mutations.Count) 个变异"
if ($survived.Count -gt 0) {
    Write-Output '存活（断言有缺口）：'
    $survived | ForEach-Object { Write-Output "  - $_" }
    exit 1
}
exit 0
