# 生成一个最小但合规的 32x32 ICO，供 tauri-build 生成 Windows 资源文件用。
#
# 为什么手写而不用图形工具：spike 只需要一个能通过校验的图标，
# 手写 ICO 头 + BMP 位图数据完全够用，不引入额外依赖。
# ICO 结构：ICONDIR(6B) + ICONDIRENTRY(16B) + BITMAPINFOHEADER(40B) + 像素 + AND 掩码
$ErrorActionPreference = 'Stop'
$out = Join-Path $PSScriptRoot 'webview-range\icons'
New-Item -ItemType Directory -Path $out -Force | Out-Null
$path = Join-Path $out 'icon.ico'

$w = 32; $h = 32
$xorSize  = $w * $h * 4              # 32bpp BGRA
$andSize  = ($w / 8) * $h            # 1bpp 掩码，行需 4 字节对齐（32/8=4，正好对齐）
$dibSize  = 40 + $xorSize + $andSize

$ms = New-Object System.IO.MemoryStream
$bw = New-Object System.IO.BinaryWriter($ms)

# ICONDIR
$bw.Write([uint16]0)      # reserved
$bw.Write([uint16]1)      # type = icon
$bw.Write([uint16]1)      # count

# ICONDIRENTRY
$bw.Write([byte]$w)
$bw.Write([byte]$h)
$bw.Write([byte]0)        # 调色板数（32bpp 为 0）
$bw.Write([byte]0)        # reserved
$bw.Write([uint16]1)      # 色彩平面
$bw.Write([uint16]32)     # 位深
$bw.Write([uint32]$dibSize)
$bw.Write([uint32]22)     # 数据偏移 = 6 + 16

# BITMAPINFOHEADER。高度写两倍：ICO 约定 XOR 图 + AND 掩码合计高度
$bw.Write([uint32]40)
$bw.Write([int32]$w)
$bw.Write([int32]($h * 2))
$bw.Write([uint16]1)
$bw.Write([uint16]32)
$bw.Write([uint32]0)      # BI_RGB
$bw.Write([uint32]($xorSize + $andSize))
0..3 | ForEach-Object { $bw.Write([uint32]0) }  # 分辨率与调色板字段

# 像素数据：自下而上。画一个蓝底 + 中间浅色方块，肉眼可辨即可
for ($y = $h - 1; $y -ge 0; $y--) {
    for ($x = 0; $x -lt $w; $x++) {
        $inner = ($x -ge 9 -and $x -lt 23 -and $y -ge 9 -and $y -lt 23)
        if ($inner) {
            $bw.Write([byte]0xEC); $bw.Write([byte]0xE8); $bw.Write([byte]0xE6)  # B G R
        } else {
            $bw.Write([byte]0xEB); $bw.Write([byte]0x63); $bw.Write([byte]0x25)
        }
        $bw.Write([byte]0xFF)  # A，全不透明
    }
}
# AND 掩码全 0 表示全部不透明
1..$andSize | ForEach-Object { $bw.Write([byte]0) }

$bw.Flush()
[System.IO.File]::WriteAllBytes($path, $ms.ToArray())
$bw.Dispose(); $ms.Dispose()

Write-Output ("已生成 {0}（{1:N0} 字节）" -f $path, (Get-Item $path).Length)
# 用 .NET 回读验证真的是合法图标，而不是只看文件存在
try {
    Add-Type -AssemblyName System.Drawing
    $ico = New-Object System.Drawing.Icon($path)
    Write-Output ("校验通过：{0}x{1}" -f $ico.Width, $ico.Height)
    $ico.Dispose()
} catch {
    Write-Output ("校验失败：" + $_.Exception.Message)
    exit 1
}
