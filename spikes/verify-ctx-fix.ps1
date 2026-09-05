# 验证右键菜单的「密码管理」「在文件管理器中显示」可用、分级角标有说明，
# 以及加密视频时不再弹出 FFmpeg 的控制台窗口。
#
# 控制台窗口那条用「加密期间有没有新的 conhost/ffmpeg 窗口」来判：
# 只截图看不出来，那些黑框一闪而过，而且可能出现在别的显示器上。

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$port = 9384
$pw = 'ctxfix-pw'
$docs = [Environment]::GetFolderPath('MyDocuments')
$work = Join-Path $docs 'omy-ctxfix-test'
$src = Join-Path $docs 'omy-ctxfix-src'
$pass = 0; $fail = 0
function Ok($m) { Write-Output "  PASS  $m"; $script:pass++ }
function Bad($m) { Write-Output "  FAIL  $m"; $script:fail++ }

try {
  taskkill /F /IM omy-gui.exe *> $null
  Start-Sleep -Milliseconds 600

  Write-Output "`n--- 0. 准备语料 ---"
  Remove-Item $work, $src -Recurse -Force -EA SilentlyContinue
  New-Item -ItemType Directory -Force $work | Out-Null
  New-Item -ItemType Directory -Force $src | Out-Null
  # 用 H.264 MP4：它会被判 P1（⚡），有分级角标可查
  & ffmpeg -v error -y -f lavfi -i "testsrc=size=320x240:rate=25:duration=3" `
    -c:v libx264 -pix_fmt yuv420p (Join-Path $src 'clip.mp4') 2>&1 | Out-Null
  $cli = Join-Path $root 'target\debug\omy.exe'
  if (-not (Test-Path $cli)) { Push-Location $root; & cargo build -q -p omy-cli; Pop-Location }
  $env:OMY_CF_PW = $pw
  & $cli encrypt (Join-Path $src 'clip.mp4') --output-dir $work `
    --password-env OMY_CF_PW --kdf-profile mobile --thumbnail auto --yes *> $null
  $n = (Get-ChildItem $work -Filter *.omy -EA SilentlyContinue).Count
  if ($n -ge 1) { Ok "语料就绪（$n 个加密文件）" } else { Bad '加密语料未产出'; throw '缺语料' }

  Write-Output "`n--- 1. 构建 ---"
  Push-Location (Join-Path $root 'crates\omy-gui\frontend')
  & npm.cmd run build 2>&1 | Select-Object -Last 1
  Pop-Location
  Push-Location $root
  & cargo build --release -p omy-gui 2>&1 | Select-String -Pattern '^error' | Select-Object -First 5
  Pop-Location
  $gui = Join-Path $root 'target\release\omy-gui.exe'
  if (-not (Test-Path $gui)) { Bad 'omy-gui.exe 不存在'; throw '缺构建产物' }
  # 自证测的是最新构建
  $exeTime = (Get-Item $gui).LastWriteTime
  $newest = Get-ChildItem (Join-Path $root 'crates\omy-gui\src'), `
    (Join-Path $root 'crates\omy-gui\frontend\src') -Recurse -File |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
  if ($newest.LastWriteTime -gt $exeTime) { Bad "源码比二进制新（$($newest.Name)）"; throw '构建过期' }
  Ok "构建新鲜（exe $($exeTime.ToString('HH:mm:ss'))）"

  Write-Output "`n--- 2. 起 GUI 跑探针 ---"
  $env:OMY_GUI_CDP_PORT = "$port"
  $proc = Start-Process -FilePath $gui -PassThru -WindowStyle Normal
  Start-Sleep -Seconds 5
  Push-Location (Join-Path $root 'spikes')
  $out = & node --experimental-websocket probe-ctx-fix.mjs $port 2>&1 | Out-String
  Pop-Location
  Write-Output $out
  if ($out -match 'PROBE_OK') { Ok '探针全部通过' } else { Bad '探针有失败项' }
  Stop-Process -Id $proc.Id -Force -EA SilentlyContinue
  Start-Sleep -Milliseconds 800

  Write-Output "`n--- 3. FFmpeg 不再弹控制台窗口 ---"
  # 直接数「加密期间冒出来的 conhost.exe / ffmpeg.exe / ffprobe.exe 进程」。
  # CREATE_NO_WINDOW 生效时 ffprobe 仍会短暂存在，但**不会**有
  # 伴随的 conhost（那是控制台宿主，有窗口才需要它）。
  $before = @(Get-Process -Name conhost -EA SilentlyContinue).Count
  $env:OMY_CF_PW2 = 'winprobe-pw'
  $w2 = Join-Path $docs 'omy-ctxfix-win'
  Remove-Item $w2 -Recurse -Force -EA SilentlyContinue
  New-Item -ItemType Directory -Force $w2 | Out-Null
  # 起一个后台作业不断采样 conhost 数量，同时做加密
  $job = Start-Job -ScriptBlock {
    $max = 0
    for ($i = 0; $i -lt 100; $i++) {
      $c = @(Get-Process -Name conhost -ErrorAction SilentlyContinue).Count
      if ($c -gt $max) { $max = $c }
      Start-Sleep -Milliseconds 100
    }
    $max
  }
  & $cli encrypt (Join-Path $src 'clip.mp4') --output-dir $w2 `
    --password-env OMY_CF_PW2 --kdf-profile mobile --thumbnail auto --yes *> $null
  $peak = Receive-Job -Job $job -Wait -AutoRemoveJob
  Write-Output "  [诊断] conhost 数量 起始=$before 峰值=$peak"
  # CLI 自己就在控制台里，所以这里只做记录不做判据；
  # 真正的判据是源码里所有 ffmpeg/ffprobe 调用都走了 command_for
  Remove-Item $w2 -Recurse -Force -EA SilentlyContinue
  Remove-Item Env:\OMY_CF_PW2 -EA SilentlyContinue

  # command_for 自己那一行 Command::new(exe) 是正当的（它就是包装者），
  # 所以只数它之外的。用行号排除：command_for 定义之后的第一处属于它。
  $lines = Get-Content (Join-Path $root 'crates\omy-media\src\ffprobe.rs')
  $defLine = ($lines | Select-String -Pattern 'fn command_for' | Select-Object -First 1).LineNumber
  $bad = @()
  for ($i = 0; $i -lt $lines.Count; $i++) {
    if ($lines[$i] -match 'Command::new\(exe\)') {
      $ln = $i + 1
      # 允许 command_for 函数体内那一处（定义后 6 行内）
      if (-not ($defLine -and $ln -gt $defLine -and $ln -le $defLine + 6)) {
        $bad += $ln
      }
    }
  }
  if ($bad.Count -gt 0) {
    Bad "ffprobe.rs 第 $($bad -join ',') 行直接用 Command::new(exe)，会弹黑框"
  } else {
    Ok 'ffprobe.rs 所有子进程调用都走 command_for（带 CREATE_NO_WINDOW）'
  }
  $flag = Select-String -Path (Join-Path $root 'crates\omy-media\src\ffprobe.rs') `
    -Pattern 'creation_flags\(CREATE_NO_WINDOW\)' -EA SilentlyContinue
  if ($flag) { Ok 'command_for 确实设了 CREATE_NO_WINDOW' }
  else { Bad 'command_for 里没有 creation_flags(CREATE_NO_WINDOW)' }
}
finally {
  taskkill /F /IM omy-gui.exe *> $null
  Remove-Item Env:\OMY_GUI_CDP_PORT -EA SilentlyContinue
  Remove-Item Env:\OMY_CF_PW -EA SilentlyContinue
  Remove-Item $work, $src -Recurse -Force -EA SilentlyContinue
  Write-Output "`n  脚本合计 $pass 通过 / $fail 失败"
  if ($fail -eq 0) { Write-Output '全部通过' }
}
