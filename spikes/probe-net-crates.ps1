# 核实局域网共享要用到的 crate 的最新版本与状态。
# 走 rsproxy sparse 索引（cargo search 在当前网络必然超时）。
function Get-CrateInfo([string]$name) {
    # sparse 索引按名字长度分层：1/2/3 字符有特殊规则，4+ 是 aa/bb/name
    $n = $name.ToLower()
    $path = switch ($n.Length) {
        1 { "1/$n" }
        2 { "2/$n" }
        3 { "3/$($n.Substring(0,1))/$n" }
        default { "$($n.Substring(0,2))/$($n.Substring(2,2))/$n" }
    }
    $url = "https://rsproxy.cn/index/$path"
    try {
        $raw = Invoke-WebRequest -Uri $url -TimeoutSec 25 -UseBasicParsing
        $lines = ($raw.Content -split "`n") | Where-Object { $_.Trim() }
        $live = @()
        foreach ($l in $lines) {
            $o = $l | ConvertFrom-Json
            if (-not $o.yanked) { $live += $o.vers }
        }
        if ($live.Count -eq 0) { return "  $name : 全部 yanked" }
        return ("  {0,-24} 最新 {1,-12} (共 {2} 个可用版本)" -f $name, $live[-1], $live.Count)
    } catch {
        return ("  {0,-24} 查询失败: {1}" -f $name, $_.Exception.Message)
    }
}

$crates = @(
    'mdns-sd',          # mDNS 服务发现
    'snow',             # Noise Protocol
    'spake2',           # PAKE
    'x25519-dalek',     # 静态密钥
    'tokio',
    'rand'
)

Write-Output '=== 候选 crate 版本 ==='
foreach ($c in $crates) { Write-Output (Get-CrateInfo $c) }
