# 音频直接预览与播放

## 结论

音频直接播放复用现有 `omystream` + HTMLAudioElement，无需新增 FFmpeg 依赖或修改加密格式。此次补齐专用播放 UI、AAC / Opus MIME、M4B 入口，并消除本地预览每次 Range 都全量读取密文的问题。

## 播放路径

文件列表打开 → PreviewOverlay 按来源生成受控 URL → OmyAudioPlayer → WebView 请求 Range → core 按块验证、解密 → 当前音频缓冲。

- 本地独立文件和本地容器条目通过 LocalFileSource 按偏移读取密文，核心 `read_source_range` 复用压缩索引与跨块拼接。
- 本地独立音视频首请求未带 Range 且超过 2 MiB 时，返回有界 206；非媒体完整 fetch 不被这项修改截断。
- 保留 `no-store`、CORS、Accept-Ranges；关闭、换源或锁定时暂停并清 src / load，不生成解密工作副本。
- 远端设备、远程位置和容器维持原来的 URL / token 授权，没有新增任意路径访问接口。
- 网络读取、响应限额不等于时长 seek 保证。能否精确拖动仍取决于容器索引及 WebView 的解码能力，例如无 seek 索引的文件可能需要更多预读。

## UI

沿用现有明暗主题与 Vue 组件，不复制移动端播放器。

1. 文件标识：音乐图标、原始文件名、后缀、实际媒体时长；未知时长显示横线，不用估算时长替代。
2. 时间轴：真实已播放位置与分段缓冲区间；不用伪造波形，也不为画波形解码整文件。
3. 操作：播放 / 暂停、前后 10 秒、原生可键盘操作的进度 range、0.5 / 0.75 / 1 / 1.25 / 1.5 / 2 倍速、音量 / 静音。
4. 状态：缓冲、播放、暂停、错误与重试。系统拒绝自动播放时保留按钮，不冒充文件损坏。
5. 生命周期：旧 play Promise 不能污染新文件；暂停和错误撤销自动锁定播放豁免。
6. 移动端：共用控件，最小 44px 命中区域、长文件名换行、底部安全区，不强制横屏。

本次不增加歌词、播放列表、后台音频服务、封面提取、音频编辑、波形解码或媒体键集成。

## MIME 与格式

- MP3：audio/mpeg。
- M4A / M4B：audio/mp4。
- AAC：audio/aac（裸 ADTS 不等于 MP4）。
- Opus：audio/ogg; codecs=opus（文件是 Ogg 容器，不是裸编码）。
- WAV、FLAC、Ogg：保留真实容器 MIME。

这些类型可进入直接预览；是否真正能播放，以当前 WebView 对容器和编码的支持为准。未知编码不通过扩展名强行宣称兼容。

## FFmpeg 是否需要改造

本次不需要。现有 `scripts/ffmpeg-build/configure-flags.sh` 已启用常见音频 decoder、demuxer、parser；但只启用 `libwebp,movtext` encoder，没有音频输出编码器。现有 P2 / P3 分级不能视为已接入播放回退。

如果后续要求 WMA、特殊 PCM 或不支持的音频容器也能直接播，需要独立落实以下边界，不能仅把一个 FFmpeg flag 打开就宣称完成。

- 只差容器时：内存转封装，保留音频码流；优先原始 Range，失败后才进入回退。
- 编码不兼容时：补目标音频 encoder 和采样率 / 声道转换能力，通过受控的密文来源解密喂给子进程，不能先解密落盘。
- 必须做可取消的输出流和时间 seek 会话；seek 时重建转换起点，不能把非 seek 的 stdin 管道伪装成任意可拖动。
- 输出需分片容器与 MSE / 相应播放适配；输出背压、内存上限、进程退出、快速换源、锁定撤销都要有覆盖。
- Android 当前没有 FFmpeg，不能静默回落到另一条不受控路径。

本版明确提示不支持实时转码，可在加密前转换成兼容格式。

## CLI

没有新增 CLI 播放命令。播放卡、进度拖动与 WebView 解码属于 GUI 交互；密文随机读取仍复用 core。GUI / CLI 覆盖表已在中英文 `site/*/guide/remote-locations.md` 登记。

## 已完成验证

- `pnpm typecheck` 通过。
- `pnpm test`：20 个测试文件、136 项通过；音频组件 7 项，覆盖 URL、HTML 转义、缓冲区间、seek 边界、音量倍速、错误重试、关闭释放、自动播放策略、异步换源。
- `pnpm build` 通过；自产 WAV 验证素材已从源码公开目录和最终构建中精确移除。
- `cargo build -p omy-gui` 通过，生成 `target/debug/omy-gui.exe`；该构建没有经过桌面原生整链播放验收。
- `python spikes/check-i18n-keys.py`：1006 个 UI 键、165 个错误键一致。
- `cargo test` 全仓库通过；新增真实协议测试对压缩与未压缩加密文件比对首段及跨块 Range，验证 2 MiB 限额、真实 MIME、no-store，以及非媒体完整 fetch 不回归。
- `cargo clippy -- -D warnings` 通过。测试链接出现 MSVC 的“正在创建库 / 对象”提示，被 rustc 标成 linker stdout warning；内容不是代码告警，未以改全局警告策略的方式掩盖它。
- `bun run build` 文档站中英文构建通过。
- 真实浏览器复用实际 Vue 音频组件，使用自产 90 秒 WAV：readyState=4，能播放（paused=false），快进后 currentTime 前进，1.5 倍速写入实际 playbackRate；在 745px 视口检查过播放卡与错误态，控件可见且无横向溢出。

## 尚未完成的验证与限制

- 桌面原生 Tauri WebView 的“真实 .omy → 列表打开 → 播放 → seek → 关闭 / 锁定”整链端到端尚未实测。桌面应用不在当前可发现的启动列表，未用 shell 或自行创建系统启动项绕过该限制。
- Android 真机未实测；iOS / macOS / Linux 解码能力未在此轮验证。
- WebDAV、Telegram、远端设备上的音频整链未实测；这次保留现有路由，仅有本地协议与共享组件验证，不能把代码共用当成网络端已验证。
- 浏览器解码实测使用 WAV；MP3、M4A / M4B、AAC、FLAC、Opus 未逐格式进行原生解码实测。

因此本次是已实现、已有单测与浏览器验证的代码变更，不宣称上述平台及来源全部完成验收。
