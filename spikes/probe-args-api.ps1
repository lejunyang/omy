$f = 'C:\Users\LJY\.cargo\registry\src\rsproxy.cn-e3de039b2554c837\tauri-runtime-wry-2.11.4\src\lib.rs'
$lines = Get-Content $f
Write-Output '--- tauri-runtime-wry 中 additional_browser_args 上下文 (5040-5070) ---'
for ($i = 5039; $i -lt 5070 -and $i -lt $lines.Count; $i++) {
    Write-Output ("{0,5}: {1}" -f ($i + 1), $lines[$i])
}

Write-Output ''
Write-Output '--- tauri 是否暴露 additional_browser_args (在 tauri crate 里搜) ---'
$reg = 'C:\Users\LJY\.cargo\registry\src\rsproxy.cn-e3de039b2554c837'
$hits = Get-ChildItem -Path (Join-Path $reg 'tauri-2.11.5\src') -Recurse -Filter '*.rs' |
    Select-String -Pattern 'additional_browser_args|browser_extensions_enabled'
foreach ($h in $hits) {
    Write-Output ("{0} : {1} : {2}" -f $h.Path.Substring($reg.Length + 1), $h.LineNumber, $h.Line.Trim())
}
