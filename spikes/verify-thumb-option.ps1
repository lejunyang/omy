# 端到端验证「加密时自选缩略图」。
#
# 真正的判据在探针之后：把 GUI 加密出来的文件用 CLI 导出缩略图，
# 比对主色。语料前 2 秒红、后 2 秒蓝，界面上选的是 3.6 秒，
# 所以必须得到蓝色。参数没传到后端时自动取 10%（0.4 秒）是红色，
# 一比颜色就露馅——只看「有没有缩略图」测不出来。

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$port = 9383
$pw = 'thumbopt-pw'
$docs = [Environment]::GetFolderPath('MyDocuments')
$src = Join-Path $docs 'omy-thumbopt-src'
$fail = 0
$pass = 0

function Ok($m) { Write-Output "  PASS  $m"; $script:pass++ }
function Bad($m) { Write-Output "  FAIL  $m"; $script:fail++ }

try {
  # 残留进程会占着调试端口，让后续每次都连到旧进程——
  # 跑的始终是改动之前的二进制，且现象极难识破
  taskkill /F /IM omy-gui.exe 2>$null | Out-Null
  Start-Sleep -Milliseconds 600

  Write-Output "`n--- 0. 准备语料 ---"
  Remove-Item $src -Recurse -Force -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force $src | Out-Null
  # 前 2 秒红、后 2 秒蓝：不同时间点的帧颜色不同，才能验证取帧生效
  $mp4 = Join-Path $src 'rb.mp4'
  & ffmpeg -v error -y `
    -f lavfi -i "color=c=red:size=320x240:duration=2:rate=25" `
    -f lavfi -i "color=c=blue:size=320x240:duration=2:rate=25" `
    -filter_complex "[0:v][1:v]concat=n=2:v=1" `
    -c:v libx264 -pix_fmt yuv420p $mp4 2>&1 | Out-Null
  if (-not (Test-Path $mp4)) { Bad '语料视频未生成'; throw '缺少语料' }
  Ok "语料就绪 $((Get-Item $mp4).Length) 字节（0~2s 红，2~4s 蓝）"

  Write-Output "`n--- 1. 构建 ---"
  Push-Location (Join-Path $root 'crates\omy-gui\frontend')
  & npm.cmd run build 2>&1 | Select-Object -Last 1
  Pop-Location
  Push-Location $root
  & cargo build --release -p omy-gui 2>&1 | Select-String -Pattern '^error' | Select-Object -First 5
  Pop-Location
  $exe = Join-Path $root 'target\release\omy-gui.exe'
  if (-not (Test-Path $exe)) { Bad 'omy-gui.exe 不存在'; throw '缺少构建产物' }

  # 自证测的是最新构建：源码比二进制新就拒绝跑，
  # 否则会拿旧二进制得出「已修好」的结论
  $exeTime = (Get-Item $exe).LastWriteTime
  $newest = Get-ChildItem (Join-Path $root 'crates\omy-gui\src'), `
    (Join-Path $root 'crates\omy-gui\frontend\src') -Recurse -File |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
  if ($newest.LastWriteTime -gt $exeTime) {
    Bad "源码比二进制新（$($newest.Name)），拒绝运行"
    throw '构建过期'
  }
  Ok "构建新鲜（exe $($exeTime.ToString('HH:mm:ss'))）"

  Write-Output "`n--- 2. 起 GUI 跑探针 ---"
  $env:OMY_GUI_CDP_PORT = "$port"
  $proc = Start-Process -FilePath $exe -PassThru -WindowStyle Normal
  Start-Sleep -Seconds 5
  Push-Location (Join-Path $root 'spikes')
  $out = & node --experimental-websocket probe-thumb-option.mjs $port 2>&1
  Pop-Location
  $out | ForEach-Object { Write-Output $_ }
  if ($out -match 'PROBE_OK') { Ok '探针全部通过' } else { Bad '探针有失败项' }

  Write-Output "`n--- 3. 关键判据：缩略图主色 ---"
  Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
  Start-Sleep -Milliseconds 800

  $omy = Get-ChildItem $src -Filter *.omy -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
  if (-not $omy) {
    Bad '没找到 GUI 加密产物'
  } else {
    $cli = Join-Path $root 'target\debug\omy.exe'
    if (-not (Test-Path $cli)) { Push-Location $root; & cargo build -q -p omy-cli; Pop-Location }
    $webp = Join-Path $env:TEMP 'omy-thumbopt.webp'
    Remove-Item $webp -Force -ErrorAction SilentlyContinue
    $env:OMY_TO_PW = $pw
    & $cli info $omy.FullName --json --password-env OMY_TO_PW --extract-thumbnail $webp *>$null
    if (-not (Test-Path $webp)) {
      Bad '导出缩略图失败——GUI 加密的文件里没有缩略图'
    } else {
      Ok "导出缩略图 $((Get-Item $webp).Length) 字节"
      # 取中心像素的 RGB
      $raw = Join-Path $env:TEMP 'omy-thumbopt.raw'
      Remove-Item $raw -Force -ErrorAction SilentlyContinue
      & ffmpeg -v error -y -i $webp -vf "crop=8:8:(iw-8)/2:(ih-8)/2,scale=1:1" `
        -f rawvideo -pix_fmt rgb24 $raw 2>&1 | Out-Null
      $b = [System.IO.File]::ReadAllBytes($raw)
      if ($b.Length -lt 3) {
        Bad '读不出缩略图像素'
      } else {
        $r = $b[0]; $g = $b[1]; $bl = $b[2]
        Write-Output "  [诊断] 中心像素 rgb($r,$g,$bl)"
        if ($bl -gt 120 -and $r -lt 90) {
          Ok "缩略图是蓝色 —— 界面上选的 3.6 秒真的生效了"
        } elseif ($r -gt 120 -and $bl -lt 90) {
          Bad "缩略图是红色 —— 取帧时间点没有传到后端（拿到的是自动取的 0.4 秒）"
        } else {
          Bad "缩略图颜色不符预期 rgb($r,$g,$bl)"
        }
      }
      Remove-Item $raw -Force -ErrorAction SilentlyContinue
    }
    Remove-Item $webp -Force -ErrorAction SilentlyContinue
    Remove-Item Env:\OMY_TO_PW -ErrorAction SilentlyContinue
  }
}
finally {
  taskkill /F /IM omy-gui.exe 2>$null | Out-Null
  Remove-Item Env:\OMY_GUI_CDP_PORT -ErrorAction SilentlyContinue
  Remove-Item $src -Recurse -Force -ErrorAction SilentlyContinue
  Write-Output "`n  脚本合计 $pass 通过 / $fail 失败"
  if ($fail -eq 0) { Write-Output '全部通过' }
}
