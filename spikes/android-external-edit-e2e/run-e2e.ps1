param(
    [string]$Avd = 'omy-test-35',
    [int]$Port = 8799,
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$stateDir = Join-Path $repo 'target\android-external-edit-e2e'
$davRoot = Join-Path $stateDir 'webdav'
$deviceFile = Join-Path $stateDir 'device.txt'
$apk = Join-Path $repo 'crates\omy-gui\gen\android\app\build\outputs\apk\universal\debug\app-universal-debug.apk'
$editorApk = Join-Path $PSScriptRoot 'build\omy-test-editor.apk'
$auth = 'Basic ' + [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes('omy:test'))

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Wait-Until([scriptblock]$Probe, [int]$Seconds, [string]$Message) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    do {
        $value = & $Probe
        if ($value) { return $value }
        Start-Sleep -Milliseconds 500
    } while ((Get-Date) -lt $deadline)
    throw $Message
}

function Adb-RunAs([string[]]$CommandArgs) {
    $output = & adb -s $script:serial shell run-as org.omy.app @CommandArgs
    if ($LASTEXITCODE -ne 0) { throw "omy run-as 失败：$($CommandArgs -join ' ')" }
    return $output
}

function Dump-Ui([string]$Name) {
    & adb -s $script:serial shell input keyevent 61 | Out-Null
    Start-Sleep -Milliseconds 300
    $remote = "/sdcard/$Name.xml"
    $local = Join-Path $stateDir "$Name.xml"
    & adb -s $script:serial shell uiautomator dump $remote | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "uiautomator dump 失败：$Name" }
    & adb -s $script:serial pull $remote $local | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "拉取 UI 树失败：$Name" }
    return [xml][IO.File]::ReadAllText($local, (New-Object Text.UTF8Encoding $false, $true))
}

function Tap-Node($Node, [string]$Label) {
    if ($null -eq $Node) { throw "未找到 UI 节点：$Label" }
    $match = [regex]::Match($Node.bounds, '\[(\d+),(\d+)\]\[(\d+),(\d+)\]')
    if (-not $match.Success) { throw "节点边界无效：$Label" }
    $x = ([int]$match.Groups[1].Value + [int]$match.Groups[3].Value) / 2
    $y = ([int]$match.Groups[2].Value + [int]$match.Groups[4].Value) / 2
    & adb -s $script:serial shell input tap $x $y | Out-Null
}

function Wait-Ui([string]$Name, [scriptblock]$Select, [int]$Seconds = 15) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    do {
        try {
            $xml = Dump-Ui $Name
            $node = & $Select $xml
            if ($null -ne $node) { return @($xml, $node) }
        } catch {}
        Start-Sleep -Milliseconds 500
    } while ((Get-Date) -lt $deadline)
    throw "等待 UI 节点超时：$Name"
}

function Read-EditorResult {
    $xmlText = (& adb -s $script:serial shell run-as org.omy.testeditor cat shared_prefs/results.xml 2>$null | Out-String)
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($xmlText)) { return $null }
    [xml]$xml = $xmlText
    $result = @{}
    foreach ($node in $xml.map.ChildNodes) {
        $name = $node.GetAttribute('name')
        if ($node.LocalName -eq 'boolean') { $result[$name] = $node.GetAttribute('value') -eq 'true' }
        elseif ($node.LocalName -eq 'long') { $result[$name] = [long]$node.GetAttribute('value') }
        else { $result[$name] = $node.InnerText }
    }
    return $result
}

function Select-TestEditor {
    # ACTION_EDIT 只有一个处理器时，系统可能跳过选择列表并直接启动；
    # 多处理器时才显示 chooser。先等直达结果，再按节点选择，兼容两种设备状态。
    $deadline = (Get-Date).AddSeconds(4)
    do {
        $direct = Read-EditorResult
        if ($direct) { return $direct }
        Start-Sleep -Milliseconds 300
    } while ((Get-Date) -lt $deadline)

    $pair = Wait-Ui 'chooser' { param($x) $x.SelectSingleNode('//node[@text="omy test editor"]') }
    Tap-Node $pair[1] 'omy test editor'
    return Wait-Until { Read-EditorResult } 15 '测试编辑器未写入结果'
}

function Open-Preview {
    & adb -s $script:serial shell am force-stop org.omy.app
    & adb -s $script:serial shell monkey -p org.omy.app -c android.intent.category.LAUNCHER 1 | Out-Null
    $pair = Wait-Ui 'home' { param($x) $x.SelectSingleNode('//node[@text="Remote locations" and @class="android.widget.Button" and @bounds!="[0,0][0,0]"]') }
    Tap-Node $pair[1] 'Remote locations'
    $pair = Wait-Ui 'places' { param($x) $x.SelectSingleNode('//node[@text="Open"]') }
    Tap-Node $pair[1] 'Open'
    $pair = Wait-Ui 'list' { param($x) $x.SelectSingleNode('//node[contains(@text,"note.txt") and @bounds!="[0,0][0,0]"]') }
    Tap-Node $pair[1] 'note.txt'
    $pair = Wait-Ui 'preview' { param($x) $x.SelectSingleNode('//node[@text="Open read-only"]') }
    return $pair[0]
}

New-Item -ItemType Directory -Force -Path $stateDir, $davRoot | Out-Null
$script:serial = if (Test-Path -LiteralPath $deviceFile) {
    [IO.File]::ReadAllText($deviceFile, (New-Object Text.UTF8Encoding $false, $true)).Trim()
} else { '' }
if ([string]::IsNullOrWhiteSpace($script:serial) -or ((& adb -s $script:serial get-state 2>$null | Out-String).Trim() -ne 'device')) {
    $script:serial = (& (Join-Path $PSScriptRoot 'start-emulator.ps1') -Avd $Avd | Select-Object -Last 1).Trim()
}

if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'build-editor.ps1')
    if ($LASTEXITCODE -ne 0) { throw '构建测试编辑器失败' }
    Push-Location (Join-Path $repo 'crates\omy-gui\frontend')
    try {
        & node .\node_modules\vue-tsc\bin\vue-tsc.js --noEmit
        if ($LASTEXITCODE -ne 0) { throw '前端类型检查失败' }
        & node .\node_modules\vite\bin\vite.js build
        if ($LASTEXITCODE -ne 0) { throw '前端生产构建失败' }
    } finally { Pop-Location }
    Push-Location (Join-Path $repo 'crates\omy-gui')
    try {
        & cargo tauri android build --debug --target aarch64
        if ($LASTEXITCODE -ne 0) { throw 'Android APK 构建失败' }
    } finally { Pop-Location }
}

Assert-True (Test-Path -LiteralPath $apk -PathType Leaf) "找不到产品 APK：$apk"
Assert-True (Test-Path -LiteralPath $editorApk -PathType Leaf) "找不到测试编辑器 APK：$editorApk"

& adb -s $script:serial install -r -d -t $apk | Out-Null
if ($LASTEXITCODE -ne 0) { throw '安装 omy APK 失败' }
& adb -s $script:serial install -r -t $editorApk | Out-Null
if ($LASTEXITCODE -ne 0) { throw '安装测试编辑器失败' }
& adb -s $script:serial shell am start -n org.omy.testeditor/.EditorActivity | Out-Null
Start-Sleep -Seconds 1

$server = $null
try {
    [IO.File]::WriteAllText((Join-Path $davRoot 'note.txt'), "original-from-webdav`n", (New-Object Text.UTF8Encoding $false))
    $existing = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue
    if (-not $existing) {
        $server = Start-Process -FilePath (Get-Command python).Source -ArgumentList @(
            (Join-Path $PSScriptRoot 'webdav_server.py'), $davRoot, '--port', [string]$Port,
            '--user', 'omy', '--password', 'test'
        ) -RedirectStandardOutput (Join-Path $davRoot 'server.stdout.log') `
          -RedirectStandardError (Join-Path $davRoot 'server.stderr.log') -PassThru
        Wait-Until {
            try {
                $r = Invoke-WebRequest -UseBasicParsing -Method Options -Uri "http://127.0.0.1:$Port/" -Headers @{ Authorization = $auth } -TimeoutSec 1
                $r.StatusCode -eq 200
            } catch { $false }
        } 15 'WebDAV 夹具未启动'
    }

    & adb -s $script:serial shell pm clear org.omy.app | Out-Null
    & adb -s $script:serial shell monkey -p org.omy.app -c android.intent.category.LAUNCHER 1 | Out-Null
    $pair = Wait-Ui 'remote-empty' { param($x) $x.SelectSingleNode('//node[@text="Remote locations" and @class="android.widget.Button" and @bounds!="[0,0][0,0]"]') }
    Tap-Node $pair[1] 'Remote locations'
    $pair = Wait-Ui 'connect' { param($x) $x.SelectSingleNode('//node[@text="Connect a remote location"]') }
    Tap-Node $pair[1] 'Connect a remote location'
    Wait-Until {
        & adb -s $script:serial shell screencap -p /sdcard/form.png | Out-Null
        $true
    } 5 '连接表单未打开' | Out-Null

    function Tap-Type([int]$x, [int]$y, [string]$value) {
        & adb -s $script:serial shell input tap $x $y | Out-Null
        Start-Sleep -Milliseconds 200
        & adb -s $script:serial shell input text $value | Out-Null
    }
    Tap-Type 540 742 'E2E'
    Tap-Type 540 925 "http://10.0.2.2:$Port/"
    Tap-Type 540 1111 'omy'
    Tap-Type 540 1296 'test'
    & adb -s $script:serial shell input keyevent 4 | Out-Null
    & adb -s $script:serial shell input tap 125 1585 | Out-Null
    & adb -s $script:serial shell input swipe 540 1700 540 900 500 | Out-Null
    & adb -s $script:serial shell input tap 915 1800 | Out-Null
    Wait-Until { ((Adb-RunAs @('cat', 'omy/config.toml')) | Out-String) -match 'name = "E2E"' } 15 'WebDAV 位置未落盘'

    $config = (Adb-RunAs @('cat', 'omy/config.toml')) | Out-String
    Assert-True (-not $config.Contains('password = "test"')) 'WebDAV 密码不得明文写入配置'
    Assert-True ($config.Contains('[remote.places.secret]')) 'WebDAV 密码信封未写入配置'

    $preview = Open-Preview
    $readonly = $preview.SelectSingleNode('//node[@text="Open read-only"]')
    $editable = $preview.SelectSingleNode('//node[@text="Open for editing"]')
    Assert-True ([regex]::Match($readonly.bounds, '\[(\d+),(\d+)\]').Groups[2].Value -as [int] -ge 80) '预览按钮仍被状态栏覆盖'

    & adb -s $script:serial shell run-as org.omy.testeditor rm -rf shared_prefs 2>$null
    Tap-Node $readonly 'Open read-only'
    $readResult = Select-TestEditor
    Assert-True ($readResult['action'] -eq 'android.intent.action.VIEW') '只读打开必须使用 ACTION_VIEW'
    Assert-True ($readResult['flag_read'] -and -not $readResult['flag_write']) '只读打开必须只授予读权限'
    Assert-True ($readResult['write_result'] -match 'SecurityException') '只读 URI 必须拒绝第三方写入'
    $stableUri = $readResult['uri']

    $preview = Open-Preview
    & adb -s $script:serial shell run-as org.omy.testeditor rm -rf shared_prefs 2>$null
    Tap-Node ($preview.SelectSingleNode('//node[@text="Open for editing"]')) 'Open for editing'
    $editResult = Select-TestEditor
    Assert-True ($editResult['action'] -eq 'android.intent.action.EDIT') '可编辑打开必须使用 ACTION_EDIT'
    Assert-True ($editResult['flag_read'] -and $editResult['flag_write']) '可编辑打开必须授予读写权限'
    Assert-True ($editResult['write_result'] -eq 'ok') '第三方编辑器必须能写入可编辑 URI'
    Assert-True ($editResult['uri'] -eq $stableUri) '同一远端文件的 URI 必须稳定不变'
    Wait-Until { [IO.File]::ReadAllText((Join-Path $davRoot 'note.txt'), (New-Object Text.UTF8Encoding $false, $true)) -eq "edited-by-omy-test-editor-v1`n" } 15 '回到前台后未立即同步到 WebDAV'

    $timed = Join-Path $stateDir 'timed.txt'
    [IO.File]::WriteAllText($timed, "timed-sync`n", (New-Object Text.UTF8Encoding $false))
    & adb -s $script:serial push $timed /data/local/tmp/timed.txt | Out-Null
    Adb-RunAs @('cp', '/data/local/tmp/timed.txt', 'files/external-edit/files/298ddc783ae322c4462646ba1c6fa0ab/note.txt') | Out-Null
    Wait-Until { ((Adb-RunAs @('ls', 'files/external-edit/files/298ddc783ae322c4462646ba1c6fa0ab')) | Out-String).Contains('note.txt.dirty') } 5 'FileObserver 未生成 dirty 标记'
    Assert-True ([IO.File]::ReadAllText((Join-Path $davRoot 'note.txt'), (New-Object Text.UTF8Encoding $false, $true)) -ne "timed-sync`n") 'FileObserver 不应实时上传'
    Wait-Until { [IO.File]::ReadAllText((Join-Path $davRoot 'note.txt'), (New-Object Text.UTF8Encoding $false, $true)) -eq "timed-sync`n" } 40 '30 秒定时同步未更新 WebDAV'

    $killed = Join-Path $stateDir 'killed.txt'
    [IO.File]::WriteAllText($killed, "recovered-after-kill`n", (New-Object Text.UTF8Encoding $false))
    & adb -s $script:serial push $killed /data/local/tmp/killed.txt | Out-Null
    Adb-RunAs @('cp', '/data/local/tmp/killed.txt', 'files/external-edit/files/298ddc783ae322c4462646ba1c6fa0ab/note.txt') | Out-Null
    & adb -s $script:serial shell am force-stop org.omy.app
    Assert-True ([IO.File]::ReadAllText((Join-Path $davRoot 'note.txt'), (New-Object Text.UTF8Encoding $false, $true)) -eq "timed-sync`n") '进程被杀前不应提前同步'
    & adb -s $script:serial shell monkey -p org.omy.app -c android.intent.category.LAUNCHER 1 | Out-Null
    Wait-Until { [IO.File]::ReadAllText((Join-Path $davRoot 'note.txt'), (New-Object Text.UTF8Encoding $false, $true)) -eq "recovered-after-kill`n" } 20 '重启后未检测并补同步未完成修改'

    $finalState = (Adb-RunAs @('cat', 'files/external-edit/state.json')) | Out-String
    Assert-True ($finalState.Contains('"dirty": false')) '最终会话仍为 dirty'
    Assert-True ($finalState.Contains('"conflict": false')) '最终会话出现冲突'
    Write-Output 'Android WebDAV 外部打开 E2E 全部通过'
} finally {
    if ($server -and -not $server.HasExited) { Stop-Process -Id $server.Id -Force }
}
