$ErrorActionPreference = 'Stop'

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$frontend = Join-Path $repo 'crates\omy-gui\frontend'
$gui = Join-Path $repo 'crates\omy-gui'

& (Join-Path $PSScriptRoot 'build-editor.ps1')
if ($LASTEXITCODE -ne 0) { throw '测试编辑器 APK 构建失败' }

$pnpmRoot = (& osdk where pnpm@9.15.1 | Select-Object -Last 1).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($pnpmRoot)) {
    throw '无法定位 osdk 管理的 pnpm@9.15.1'
}
$pnpmCli = Join-Path $pnpmRoot 'bin\pnpm.cjs'
if (-not (Test-Path -LiteralPath $pnpmCli -PathType Leaf)) {
    throw "找不到 pnpm CLI：$pnpmCli"
}

$previousCi = $env:CI
$env:CI = 'true'
Push-Location $frontend
try {
    & node $pnpmCli install --frozen-lockfile
    if ($LASTEXITCODE -ne 0) { throw '安装前端依赖失败' }
    & node $pnpmCli typecheck
    if ($LASTEXITCODE -ne 0) { throw '前端类型检查失败' }
    & node $pnpmCli test
    if ($LASTEXITCODE -ne 0) { throw '前端测试失败' }
    & node $pnpmCli build
    if ($LASTEXITCODE -ne 0) { throw '前端生产构建失败' }
} finally {
    Pop-Location
    $env:CI = $previousCi
}

Push-Location $gui
try {
    & cargo tauri android build --debug --target aarch64
    if ($LASTEXITCODE -ne 0) { throw 'Android debug APK 构建失败' }
} finally {
    Pop-Location
}

$apk = Join-Path $gui 'gen\android\app\build\outputs\apk\universal\debug\app-universal-debug.apk'
if (-not (Test-Path -LiteralPath $apk -PathType Leaf)) {
    throw "构建成功但找不到 APK：$apk"
}
Write-Output $apk
