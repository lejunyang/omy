# 查询 crates.io 版本。
#
# cargo search 在当前网络下必然超时（镜像的 source.replace-with 只作用于
# 依赖解析，不影响 registry API），所以走 rsproxy 的 sparse 索引。
# 索引路径按包名长度分层：1 字符 -> /1/{name}，2 -> /2/{name}，
# 3 -> /3/{首字母}/{name}，>=4 -> /{前2}/{3-4}/{name}
param([string[]]$Names)

function Get-IndexPath([string]$n) {
    $l = $n.ToLower()
    switch ($l.Length) {
        1 { return "1/$l" }
        2 { return "2/$l" }
        3 { return "3/$($l.Substring(0,1))/$l" }
        default { return "$($l.Substring(0,2))/$($l.Substring(2,2))/$l" }
    }
}

foreach ($n in $Names) {
    $url = "https://rsproxy.cn/index/" + (Get-IndexPath $n)
    try {
        $raw = Invoke-WebRequest -Uri $url -TimeoutSec 30 -UseBasicParsing
        $lines = ($raw.Content -split "`n") | Where-Object { $_.Trim().Length -gt 0 }
        # 取最后一个非 yanked 版本
        $latest = $null
        for ($i = $lines.Count - 1; $i -ge 0; $i--) {
            $o = $lines[$i] | ConvertFrom-Json
            if (-not $o.yanked) { $latest = $o; break }
        }
        if ($latest) {
            Write-Output ("{0,-28} {1}" -f $n, $latest.vers)
        } else {
            Write-Output ("{0,-28} 无可用版本" -f $n)
        }
    } catch {
        Write-Output ("{0,-28} 查询失败: {1}" -f $n, $_.Exception.Message)
    }
}
