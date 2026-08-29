# 查 wry 是如何启用 WebView2 调试端口的：
# TAURI_REMOTE_DEBUGGING_PORT 没生效，需要确认是构建配置问题还是别的。
$reg = 'C:\Users\LJY\.cargo\registry\src\rsproxy.cn-e3de039b2554c837'
Set-Location $reg

Write-Output '--- wry 版本 ---'
Get-ChildItem -Directory -Filter 'wry-*' | Select-Object -ExpandProperty Name

Write-Output ''
Write-Output '--- wry 中与调试端口/额外参数相关的代码 ---'
$hits = Get-ChildItem -Path 'wry-*\src' -Recurse -Filter '*.rs' |
    Select-String -Pattern 'ADDITIONAL_BROWSER_ARGUMENTS|remote-debugging-port|additional_browser_args|AdditionalBrowserArguments'
foreach ($h in $hits) {
    $rel = $h.Path.Substring($reg.Length + 1)
    Write-Output "$rel : $($h.LineNumber) : $($h.Line.Trim())"
}

Write-Output ''
Write-Output '--- tauri 中 devtools / 调试端口相关 ---'
$hits2 = Get-ChildItem -Path 'tauri-2.11.5\src' -Recurse -Filter '*.rs' |
    Select-String -Pattern 'REMOTE_DEBUGGING|devtools\(' 
foreach ($h in $hits2) {
    $rel = $h.Path.Substring($reg.Length + 1)
    Write-Output "$rel : $($h.LineNumber) : $($h.Line.Trim())"
}

Write-Output ''
Write-Output '--- tauri-runtime-wry 中 devtools 开关 ---'
$hits3 = Get-ChildItem -Path 'tauri-runtime-wry-*\src' -Recurse -Filter '*.rs' -ErrorAction SilentlyContinue |
    Select-String -Pattern 'with_devtools|devtools'
foreach ($h in $hits3 | Select-Object -First 25) {
    $rel = $h.Path.Substring($reg.Length + 1)
    Write-Output "$rel : $($h.LineNumber) : $($h.Line.Trim())"
}
