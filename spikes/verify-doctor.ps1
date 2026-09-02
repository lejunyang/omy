# 交叉核对 doctor 的媒体探测结论是否与本机真实情况一致。
#
# 用不同于产品的路径来查（Get-Command 走 PATH），而不是复述
# doctor 自己的输出——用同一条路径验证等于没验证。

$ErrorActionPreference = 'Continue'
$omy = 'E:\Projects\omy\target\debug\omy.exe'

Write-Output '=== doctor 的说法 ==='
$out = & $omy doctor 2>&1 | Out-String
$mediaLine = ($out -split "`n" | Where-Object { $_ -match '媒体预览' })
$lanLine = ($out -split "`n" | Where-Object { $_ -match '局域网' })
Write-Output "  $($mediaLine.Trim())"
Write-Output "  $($lanLine.Trim())"

Write-Output ''
Write-Output '=== 本机真实情况（走 PATH，不经过产品代码）==='
$probeReal = [bool](Get-Command ffprobe -ErrorAction SilentlyContinue)
$ffReal = [bool](Get-Command ffmpeg -ErrorAction SilentlyContinue)
Write-Output "  ffprobe 存在: $probeReal"
Write-Output "  ffmpeg  存在: $ffReal"

Write-Output ''
Write-Output '=== 一致性判定 ==='
$claimsBoth = $mediaLine -match '均可用'
$claimsNone = $mediaLine -match '未找到'
$pass = $true

if ($probeReal -and $ffReal) {
    if (-not $claimsBoth) { Write-Output '  FAIL 两个都在，doctor 却没报「均可用」'; $pass = $false }
    else { Write-Output '  PASS 两个都在，doctor 报「均可用」' }
} elseif (-not $probeReal -and -not $ffReal) {
    if (-not $claimsNone) { Write-Output '  FAIL 两个都不在，doctor 却没报「未找到」'; $pass = $false }
    else { Write-Output '  PASS 两个都不在，doctor 报「未找到」' }
} else {
    if ($mediaLine -notmatch '但缺') { Write-Output '  FAIL 只有一个在，doctor 应报「但缺 …」'; $pass = $false }
    else { Write-Output '  PASS 只有一个在，doctor 报了缺哪个' }
}

# 局域网：share 子命令真的能跑，doctor 就不能说「未实现」
$shareOk = $false
& $omy share --help *> $null
if ($LASTEXITCODE -eq 0) { $shareOk = $true }
Write-Output "  share 子命令可用: $shareOk"
if ($shareOk -and $lanLine -match '未实现') {
    Write-Output '  FAIL share 能跑，doctor 却说局域网「未实现」'
    $pass = $false
} else {
    Write-Output '  PASS 局域网结论与 share 的真实可用性一致'
}

Write-Output ''
Write-Output '=== JSON 模式的结论必须与人类可读一致 ==='
$json = & $omy --json doctor 2>&1 | Out-String
if ([string]::IsNullOrWhiteSpace($json)) {
    Write-Output '  FAIL JSON 输出为空'
    $pass = $false
} else {
    try {
        $o = $json | ConvertFrom-Json
        $m = $o.checks | Where-Object { $_.name -match '媒体预览' }
        $l = $o.checks | Where-Object { $_.name -match '局域网' }
        Write-Output "  媒体: status=$($m.status)"
        Write-Output "  局域网: status=$($l.status)"
        if ($l.status -ne 'ok') { Write-Output '  FAIL 局域网在 JSON 里不是 ok'; $pass = $false }
        else { Write-Output '  PASS JSON 里局域网为 ok' }
        # 两种输出若结论不同，脚本消费者与人看到的会是两回事
        $mediaOkText = $claimsBoth
        $mediaOkJson = ($m.status -eq 'ok')
        if ($mediaOkText -ne $mediaOkJson) {
            Write-Output '  FAIL 媒体结论在文本与 JSON 间不一致'
            $pass = $false
        } else {
            Write-Output '  PASS 媒体结论在两种输出间一致'
        }
    } catch {
        Write-Output "  FAIL JSON 解析失败: $_"
        $pass = $false
    }
}

Write-Output ''
if ($pass) { Write-Output '=== 全部一致 ==='; exit 0 }
Write-Output '=== 存在不一致 ==='
exit 1
