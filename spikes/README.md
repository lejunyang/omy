# Spikes：可行性验证

这里的程序不是产品代码，用途是**在动手实现前用真实环境证伪关键假设**。
每个 spike 都要能给出机器可判定的结论，而不是「看起来能用」。

## 为什么不能只读文档

WebView2 / WKWebView / WebKitGTK 对自定义协议、Range 请求、缓存策略的
实际行为与文档常有差异，且随版本变化。S1 的结论直接决定 GUI 的播放方案
（自定义协议 vs 本地 HTTP server），选错的返工成本很高。

## 已完成

### S1：自定义协议能否支撑 `<video>` 任意 seek

**结论：✅ 通过（16/16 断言）**

在 Windows / WebView2 151.0.4129.107 上实测：

| 验证项 | 结果 |
|---|---|
| 6 次乱序 seek（85%/15%/60%/5%/95%/35%） | 全部触发 `seeked`，落点偏差 0.00s，1~129 ms |
| seek 后数据可用性 | `readyState` 均为 4（HAVE_ENOUGH_DATA） |
| canvas 取帧（5/30/50 秒） | 503/538/529 种颜色，三帧特征互不相同 |
| 无 Range 请求 | 200 + 完整长度 |
| `bytes=0-1023` | 206 + 恰好 1024 字节 |
| `bytes=-512`（suffix 形式） | 206 + `Content-Range: bytes 4459850-4460361/4460362` |
| 越界 `bytes=999999999-` | 416 + `bytes */4460362` |
| 磁盘缓存命中 | `fromDiskCache` 全程 0 |

**canvas 取帧是排除假通过的关键**：`seeked` 事件触发不等于画面真的变了，
只有取到不同的像素才能证明解码链路完整。测试视频叠加了时间码水印，
不同位置的画面内容必然不同。

### S5：WebView 是否把解密后的明文写入磁盘

**结论：✅ 通过，但有前提**

从明文构造三段特征字节（ftyp 头 / 50% / 85% 处），播放并 seek 后扫描
WebView2 缓存目录、Edge 缓存目录与 Temp，只检查新增或变化的文件。
未发现任何明文特征。

**前提**：响应头带了 `Cache-Control: no-store, no-cache, must-revalidate`。
这只证明「带上时不落盘」，**不证明「不带也不落盘」**——产品代码必须
保留这些头。

探针选取有讲究：要求 16 字节内不同字节数 ≥ 10，避免选到 MP4 的填充区
导致假阴性（全零片段在任何文件里都能匹配上）。

## 复现

```powershell
cargo build --release -p omy-spike-webview
pwsh -NoProfile -ExecutionPolicy Bypass -File spikes\run-spike.ps1
```

全自动，无需人工点击。流程：记录缓存基线 → 启动带 CDP 的 spike →
用 CDP 驱动全部测试 → 关窗取统计 → 扫描缓存。

首次运行需要测试素材：

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File spikes\make-fixture.ps1
```

生成 60 秒 640x360 带时间码水印的 MP4 并用 omy 加密。素材不入库
（可脚本重建），`tools/ffmpeg` 由 `scripts\fetch-ffmpeg.ps1` 下载。

## 为什么用 CDP 而不是 GUI 自动化

WebView2 基于 Chromium，启动时带 `--remote-debugging-port` 即开放 CDP。
相比截图点击：

- 能拿到确定的状态码、响应头、`video.error.code`，而不是靠截图推断
- 能读 `duration` / `currentTime` / `buffered` / `readyState` 的真实值
- 能取 canvas 像素，这是判断「画面真的变了」的唯一自动化手段
- 可重复执行，修 bug 后立刻回归

`mini-ws.mjs` 是自写的最小 WebSocket 客户端：Node 20 的全局 `WebSocket`
需要 `--experimental-websocket`，而 `ws` 包需联网安装。CDP 只用文本帧，
用 `net` 模块实现握手与掩码帧即可，零依赖且不受 Node 版本影响。

## 独立验证通道

`verify-decrypt-chain.ps1` 用 **CLI + ffprobe** 走一条完全不同于 spike
的路径验证解密链：整体解密后比对 SHA256、用 `omy cat --range` 模拟分段
请求、用 ffprobe 确认产物是可解码的 h264/aac MP4、检查 ftyp 品牌。

调试 S1 时它起了决定性作用——先用它确认「解密链路本身没问题」，
才能把故障范围锁定在 WebView 协议层，避免在错误方向上排查。

## 修掉的缺陷

这些都会在产品代码里复现，已记入 `PROGRESS.md`：

1. **`fetch_ct` 偏移语义用错** —— 回调收到的是相对载荷起点的偏移，
   当成文件绝对偏移会导致 `ChunkAuthFailed`，而报错指向「数据损坏」，
   极易误判。已在 `payload.rs` 补 3 项测试固化。
2. **缺 CORS 头** —— 页面 origin 是 `http://tauri.localhost`，协议是
   `omystream://localhost`，不同源。`<video>` 不检查 CORS 所以能播，
   但 `fetch` 被拦、canvas 被污染。
3. **`OPTIONS` 预检触发全量解密** —— 预检被当 GET 处理。
4. **开放结尾 Range 导致近全量解密** —— WebView 发 `bytes=1572864-`，
   返回到末尾等于放弃按需解密。已限制单次 2 MiB。

## 未开始

S2（移动端自定义协议）、S3（Argon2 移动端耗时与内存）、
S4（mDNS 跨平台可靠性）、S6（大目录扫描性能）、
S7（FFmpeg 移动端体积）、S8（分片跨设备合并）。
