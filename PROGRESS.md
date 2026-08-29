# omy 实现进度

> 本文件是**跨会话的权威状态来源**。每完成一个可验证的阶段就更新，
> 并随代码一起提交，以便任何时候都能接续。
>
> 最后更新：2026-08-29

## 状态速览

| crate | 状态 | 测试 | 说明 |
|---|---|---|---|
| `omy-core` | 🟢 格式核心可用 | 130 项 | 格式读写、密钥、分块、分片、原子写、扫描、容器、BlockSource |
| `omy-cli` | 🟢 12 个命令可用 | 46 项 + 76 项端到端 | 契约见 `docs/research/09-cli-design.md` |
| `omy-media` | 🟢 探测/分级/moov/缩略图可用 | 71 项 + 36 项真实文件验证 | LGPL，FFmpeg 封装 |
| `omy-net` | ⚪ 未开始 | — | mDNS + SPAKE2 + Noise IK |
| `omy-gui` | ⚪ 未开始 | — | Tauri v2；**播放方案已由 S1 验证可行** |

合计 **253 项自动化测试 + 76 项 CLI 端到端断言 + 36 项 omy-media 真实文件断言 + 16 项 Spike 断言**，`cargo clippy --workspace --all-targets -- -D warnings` 零告警。

### Spike 结论

| Spike | 结论 | 影响 |
|---|---|---|
| S1 自定义协议 206 + seek | ✅ **通过**（16/16） | 不需要本地 HTTP server，省掉端口占用与 CORS 复杂度 |
| S5 WebView 缓存泄露 | ✅ **通过**（有前提） | 必须保留 `Cache-Control: no-store` 等响应头 |
| S2/S3/S4/S6/S7/S8 | ⚪ 未开始 | — |

复现：`pwsh -NoProfile -ExecutionPolicy Bypass -File spikes\run-spike.ps1`
（全自动，无需人工点击；需先 `cargo build --release -p omy-spike-webview`）

#### S1 实测数据

- 6 次乱序 seek（85%/15%/60%/5%/95%/35%）全部触发 `seeked`，落点偏差 **0.00s**，耗时 1~129 ms，`readyState` 均为 4
- canvas 在 5/30/50 秒取到 503/538/529 种颜色，三帧特征互不相同 → 排除「seek 报成功但画面卡住」的假通过
- Range 语义：无 Range → 200；`bytes=0-1023` → 206/1024B；`bytes=-512` → 206/512B 且 `Content-Range: bytes 4459850-4460361/4460362`；越界 → 416 + `bytes */total`
- `fromDiskCache` 全程为 0

#### 产品实现必须遵守的约束（由 S1/S5 实测得出）

1. **响应头必须带 `Cache-Control: no-store, no-cache, must-revalidate` 与 `Pragma: no-cache`**。S5 只证明「带上时不落盘」，未证明「不带也不落盘」。
2. **必须回 CORS 头**。页面 origin 是 `http://tauri.localhost`，自定义协议是 `omystream://localhost`，不同源。`<video>` 不检查 CORS 所以能播，但 `fetch` 会被拦、canvas 会被污染——图片预览、文本读取、缩略图生成都依赖这两者。且必须 `Access-Control-Expose-Headers: Content-Range`，否则 JS 读不到它。
3. **`<video>` / `<img>` 要加 `crossorigin="anonymous"`**，否则即使服务端放行，canvas 取帧仍报污染。
4. **必须提前处理 `OPTIONS` 预检并返回 204**，不能落入解密路径——实测预检曾触发整个文件解密。
5. **必须限制单次响应上限**。WebView 的 seek 请求是开放结尾（`bytes=1572864-`），返回到文件末尾等于近全量解密。当前上限 2 MiB；产品应按码率与块大小调整，并考虑对齐块边界避免同块重复解密。无 Range 的 200 响应仍须返回完整内容。
6. **协议处理器必须用异步版**（`register_asynchronous_uri_scheme_protocol`），同步版会阻塞 WebView 线程。


## 已完成

### omy-core 格式核心（提交 `ec5676a`）

| 模块 | 职责 |
|---|---|
| `crypto` | 两级 KDF（Argon2id → HKDF）、AEAD 抽象、密钥析构擦除 |
| `header` | Fixed Header 编解码、O(1) 块偏移、`argon2_params()` |
| `slot` | Key Slot Area，未用槽填随机字节 |
| `tlv` | TLV 编解码、文件名 64 B 桶填充、压缩索引 |
| `payload` | 分块 AEAD、任意区间随机访问 |
| `shard` | 切分/合并/冗余头恢复/缺片空洞分析 |
| `file` | 顶层封装、容器 FOLDER_INDEX |
| `error` | 结构化错误码 + CLI 退出码映射 |

### omy-core 运行时（提交 `27b0331`）

| 模块 | 职责 |
|---|---|
| `fsatomic` | 原子写入、临时明文擦除、`mark_dir_no_index`、清理残留 |
| `session` | KEK 会话缓存、空闲自动上锁、恢复码 |
| `scan` | 目录扫描、多密码匹配、统计 |

### omy-core 容器与数据源（本轮）

| 模块 | 职责 | 测试 |
|---|---|---|
| `container` | 目录容器索引：紧凑二进制编码、路径安全校验、区间自洽校验 | 19 项 |
| `source` | `BlockSource` trait + 内存/本地文件/容器条目视图 | 6 项 |

设计要点：

- **路径用数组而非拼接字符串**，天然规避分隔符差异与路径注入。
- **解析阶段就拒绝**空串 / `.` / `..` / 含分隔符 / 含 NUL / 含冒号 / 超 255 字节的组件，不留给调用方自觉。
- 自定义紧凑二进制而非 JSON：索引可达 10 MB 量级且要在移动端解析。
- `BlockSource` **有意做成同步 trait**（设计文档草案是 `#[async_trait]`）：`payload::read_range` 是纯 CPU 解密逻辑，异步化会污染整条调用链；core 保持 MIT/Apache 轻量、不引入 async 运行时；远程实现在 omy-net 内部自行桥接异步。已在模块 doc 注明。
- `FOLDER_INDEX` TLV 标为 **CRITICAL**：不认识该 TLV 的实现必须拒绝打开，否则会把容器误当普通文件解出一堆拼接字节。

### omy-cli（本轮）

12 个子命令全部可用：

| 命令 | 状态 | 要点 |
|---|---|---|
| `encrypt` | ✅ | 批量复用同一 KEK（Argon2 只跑一次）；`--delete-original` 先真解密比对再删 |
| `decrypt` | ✅ | 容器展开含二次路径校验 + Windows 非法字符/保留名处理并报告调整项 |
| `info` | ✅ | **slot 占用恒显示"未知"**，JSON 里 `slot_count` 恒为 null |
| `verify` | ✅ | 两阶段：无密码验结构 / 有密码验全量 |
| `list` | ✅ | 目录内 omy 文件清单 |
| `scan` | ✅ | 先探测 `(salt, Argon2 参数)` 组合再派生 KEK 再扫描 |
| `cat` | ✅ | 按块流式，内存恒定；支持 `--range` |
| `shard` | ✅ | `split` / `merge` / `check`，缺片报退出码 6 |
| `bench` | ✅ | 本机实测 KDF 与 AEAD；debug 构建会警告数字无效 |
| `doctor` | ✅ | 环境自检，如实报告未实现能力 |
| `completion` | ✅ | bash / zsh / fish / powershell |
| `key list` | ✅ | — |
| `key add/remove/change` | ⛔ 如实报未实现 | core 缺"保留 FEK、原地重建 slot 区、重算头部 MAC 且不重写载荷"的能力 |
| `encrypt --mode tree` | ⛔ 如实报未实现 | 需目录名加密与 base32 编码（文档 05 §3） |

模块设计要点：

- `password.rs`：五种通道 + **在 `main` 里前置拦截 `--password`**（clap 报"未知参数"对用户无帮助，而这是最易误用、后果最重的旁路 L12）。非终端时拒绝交互并指明替代；Unix 下密码文件权限过宽时警告；`first_line` 保留前导空格（密码可以空格开头）。
- `output.rs`：stdout 只放结果数据、stderr 放进度与提示，保证 `omy cat x | mpv -` 与 `omy info --json | jq` 不被污染。`report_error` **遍历整个错误链**取 code 与退出码。
- `i18n.rs`：用 `catalog!` 宏同时生成两语言查表以保证 key 不漏；缺失 key 返回 key 本身而非 panic。
- `config.rs`：`deny_unknown_fields` 让拼错的键报错而非静默忽略；显式 `--config` 读不到必须报错，默认路径不存在则用内置默认。

## 验证

### 独立验证通道（提交 `217df52`）

不采信与实现同源的自测，另开四条独立通道：

| 工具 | 手段 | 结果 |
|---|---|---|
| `crash_writer` + `scripts/verify-atomic-write.ps1` | 真实 `process::abort()` 杀进程 | 11/11。崩溃不产生半成品；覆盖写崩溃时原文件完好 |
| `bench_scan` | 生产参数实测 | Argon2id 91.2 ms、HKDF 0.30 µs；300 文件精确解锁 100 个，75 个噪音零误判 |
| `fuzz_parse` | 8 策略 × 20,000 次随机变异 | 零 panic |
| `audit_mac_scope` | 逐字节翻转 | 头部 656 B 全被 MAC 拒绝、载荷 3016 B 全被 AEAD 拒绝，均零漏网 |

### CLI 端到端（本轮，`scripts/verify-cli.ps1`）

用**真实编译出的二进制**走完整流程，76 项断言全通过。单元测试无法验证"命令行参数解析 + 进程退出码 + stdout/stderr 分工"，这些只能靠真实调用二进制。

覆盖：拒绝明文密码参数、加解密逐字节往返、退出码契约（2/3/4/6）、JSON 错误格式、cat 管道与范围读取、相同内容不同密码文件大小一致、list/scan、分片切分/缺片检测/合并哈希一致、目录容器嵌套与空目录还原、doctor/bench/completion、帮助与版本。

**这一轮端到端验证发现了 3 个单元测试没发现的真实缺陷**（见下节）。

### 本机实测性能（release，2026-08-29）

| 项 | 数值 |
|---|---|
| Argon2id mobile（32 MiB, t=4） | 46.9 ms |
| Argon2id interactive（64 MiB, t=3） | 75.4 ms |
| HKDF（每文件每 slot） | 0.288 µs |
| 比值 | 约 262,000× |
| ChaCha20-Poly1305 | 加密 1.35 GB/s、解密 1.31 GB/s |
| AES-256-GCM | 加密 1.55 GB/s、解密 1.57 GB/s（本机有 AES-NI） |

⚠️ **debug 构建的数字毫无参考价值**：实测 AEAD 吞吐差约 130 倍（0.01 vs 1.35 GB/s）、HKDF 差约 23 倍。`bench` 命令已在 debug 下主动警告。

### 复现命令

```bash
cargo test --workspace                            # 253 项
cargo clippy --workspace --all-targets -- -D warnings   # 零告警
cargo build --release -p omy-cli
pwsh -File scripts/verify-cli.ps1                 # 76 项端到端
./target/release/omy bench                        # 本机性能
cargo run --release --example bench_scan -- 300
cargo run --release --example fuzz_parse -- 20000
cargo run --release --example audit_mac_scope
cargo build --example crash_writer && pwsh -File scripts/verify-atomic-write.ps1

# omy-media：先生成素材，再跑真实文件验证（36 项）
pwsh -File spikes/make-media-fixtures.ps1
cargo run --release --example verify_media -p omy-media
```

### omy-media 真实文件验证（36 项）

单元测试用的是**手写的 ffprobe JSON 样本**，只能验证"给定这段 JSON 能否正确解析"，
无法验证"真实 ffprobe 的输出是否真的长这样"。两者同源时，对格式的误解会同时存在于
样本和实现里——这正是缺陷 #6 漏过 76 项端到端验证的原因。

因此 `examples/verify_media.rs` 用**真实媒体文件 + 真实 ffprobe** 独立验证，
每个断言的期望值按文档 §5.2 人工判定，而非"实现算出什么就认什么"。

素材（`spikes/make-media-fixtures.ps1` 生成 10 个）刻意覆盖易错组合：

| 素材 | 验证点 |
|---|---|
| `h264_aac.mp4` | P1 基线；且**moov 在 99% 处**，是缺陷 #7/#8 的触发源 |
| `h264_aac.mkv` | 容器不被 `<video>` 支持但可 remux → P2 |
| `vp9_opus.webm` | 真 WebM 可直通 → P1 |
| `mpeg4_mp3.avi` | MPEG-4 ASP 两端都不支持 → P3 |
| `h264_flac.mkv` | FLAC 原生可解但进不了 MP4 |
| `with_cover.mp3` | 带封面 → 必须**不**被判成视频（`attached_pic`） |
| `not_media.txt` | 必须返回可降级的 `NOT_MEDIA` 而非崩溃 |
| `h264_aac_faststart.mp4` | 与尾部 moov 版本对照，锁定重排正确性 |

关键证据：尾部 moov 素材经纯 Rust 重排后抽帧得到 **4404 字节**，
与直接用文件路径抽帧的结果**完全一致**——这是偏移修正正确的最强证据。

## 本轮修复的真实缺陷

端到端验证与 clippy 严格门禁各暴露出必须修的问题：

| # | 缺陷 | 根因 | 修复 |
|---|---|---|---|
| 1 | `scan` 解锁数恒为 0 | 硬编码 `Argon2Params::INTERACTIVE`，但文件可能用任意档位加密，参数不符必然派生出错误 KEK | 改为**成对**取自同一文件头的 `(vault_salt, argon2_params)`；label 附带参数指纹避免同 salt 不同参数时缓存串用 |
| 2 | 缺片退出码退化为 1 | `report_error` 只 `downcast_ref` 顶层，经 `.context()` 包装后取不到 | 改为遍历整个错误链；补测试锁定多层包装场景 |
| 3 | 缺片时 `shard check` 报格式错误而非缺片 | `load_parts` 遇缺失序号立刻 `break`，只读到缺口之前的片，`shard_total` 与片数矛盾 | 改为容忍连续 64 个缺号；`decrypt` 里同一处一并修正 |
| 4 | 盘符校验漏判且逻辑不对 | `c.as_bytes()[1] == b':'` 只看第 2 字节，多字节 UTF-8 下冒号不在下标 1 就漏过 | 改用字符迭代判断；并**拒绝任意位置的冒号**（Windows 上 `a.txt:hidden` 会创建 NTFS 备用数据流） |
| 5 | 超长多字节字符串编码会 panic | `&root[..root_len]` 在 65535 处按字节切，切断多字节字符即 panic（中文每字符 3 字节，必然命中） | 新增 `truncate_utf8` 按字符边界回退；root / 路径组件 / target / xattr key 全部改用；补测试实际触发验证 |
| 6 | `cat --range` 每个区间少一个字节 | `parse_range` 返回 `(start, end)` 却未定义开闭区间，调用处按开区间处理，用户按 HTTP Range 惯例期望闭区间。**76 项端到端验证没抓到**，因为断言写成"`0-100` 输出 100 字节"，恰好用开区间的期望值匹配了开区间的实现——测试与实现同错、互相掩护 | `parse_range` 改为返回 `(offset, length)` 让调用方不可能弄错；同时修正 `-N` 语义（原为"前 N 字节"，与 HTTP `bytes=-N` 的"最后 N 字节"相反）；污染的断言改为 `0-99 → 100 字节`，补单字节/末字节/suffix 三类边界 |
| 7 | 尾部 moov 的 MP4 探测与抽帧全部失败 | 管道输入无法 seek，FFmpeg 读到 mdat 就报 `partial file` + `Cannot determine format after EOF`，stdout 为空。让 FFmpeg 自己 `-movflags frag_keyframe+empty_moov` remux 也只产出 1301 字节空壳（它同样读不到 moov）。而录屏、相机直出、`-c copy` 输出**默认都是尾部 moov** | 新增 `mp4::to_faststart`：在主进程内用**纯 Rust** 把 moov 前移并修正 `stco`/`co64` 偏移，再喂管道。既解决问题又不违反文档 §14「FFmpeg 子进程无文件系统访问」 |
| 8 | 同一个 stco 被登记 16 次，偏移累加 16 遍 | `collect_offset_tables` 递归进容器时**没有收窄搜索上界**，子调用一直扫到 moov 末尾，把容器之外的 stco 又扫一遍。嵌套 trak/mdia/minf/stbl 四层就重复多次。实测首项从应有的 4611 变成 63930，FFmpeg 报 `Invalid NAL unit size (1593407596 > 5369)`。**单元测试没抓到，因为手写样本只有一层嵌套、单条轨道** | 递归时传入 `end` 上界并收窄到当前容器末尾；补三条回归测试（不得重复登记、双轨恰好 2 个表、双轨端到端逐项核对偏移只加一次） |
| 9 | 误把 FFmpeg 的 stderr 当成有效产物 | 诊断脚本用 `> out 2>&1` 把 stdout 与 stderr 混进同一文件，又只检查"长度 > 100"，于是 844 字节的错误文本被判定为成功抽帧，据此得出「尾部 moov 也能抽帧」的错误结论，并按错误结论改了实现 | 校验产物必须看**内容特征**：`image_decodable` 先验 WebP/JPEG/PNG 魔数再真实解码；诊断时 stdout 与 stderr 必须分开重定向 |

## 待办

### core 缺口

- [x] `container` 目录容器索引
- [x] `BlockSource` trait
- [ ] `tree` 模式：目录名加密 + base32 编码（文档 05 §3）
- [ ] slot 原地增删改：保留 FEK、重建 slot 区、重算头部 MAC，**不重写载荷**（`key add/remove/change` 依赖此能力）
- [ ] 元数据保存与还原报告的完整实现（D-20 / N3）
- [ ] 伪装模式：`footer` / `host-jpeg` / `host-png`（D-17）
- [ ] 符号链接跨平台还原（当前如实报告为跳过）

### omy-cli

- [x] 12 个子命令
- [x] 密码五通道 + 拒绝 `--password`
- [x] `--json` 输出；`code` 恒为英文常量
- [x] i18n 简中 + 英文
- [x] 配置文件
- [ ] `serve` / `connect`（依赖 omy-net）
- [ ] 进度条（`indicatif` 已在 workspace deps 但未接入；大文件加密目前无进度反馈）

### omy-media

- [x] `ffprobe`：探测 ffprobe/ffmpeg 可用性、管道运行、超时强杀
- [x] `probe`：容器/流解析，容忍字符串-数字-缺失-`N/A` 四态；排除 `attached_pic`
- [x] `mp4`：顶层 box 解析、moov 定位、**faststart 重排（含 stco/co64 偏移修正）**
- [x] `tier`：P1/P2/P3 播放分级，区分真 WebM 与 MKV
- [x] `thumbnail`：图片走纯 Rust `image`，视频走 FFmpeg 抽帧
- [ ] 与 core 打通：写入 `TLV_MEDIA_META` / `TLV_MOOV_CACHE` / `TLV_THUMBNAIL`
- [ ] remux 到 MSE 可用的分片 MP4（P2 路径）
- [ ] 转码选项（加密时可选转 web 原生格式）
- [ ] 字幕轨提取（首期只做文本类，ASS/PGS 留接口）

### 后续

- [ ] `omy-net`：mDNS + SPAKE2 + Noise IK
- [ ] `omy-gui`：Tauri v2
- [ ] Spike S2/S3/S4/S6/S7/S8（S1、S5 已通过，见上文「Spike 结论」）

## 已定决定

| 项 | 决定 |
|---|---|
| 项目名 / 后缀 | `omy` / `.omy` |
| MAGIC | `"OMYFILE" + 0x01`（已确认不改，固化于代码、5 组向量、格式文档） |
| 许可 | core/net：MIT OR Apache-2.0；media：LGPL-2.1+；cli/gui：GPL-3.0 |
| 首期语言 | 简体中文 + 英文 |
| 字幕 | ASS/SSA/PGS/VobSub 暂不实现，已在文档标注；外挂字幕随文件一起加密并提示 |
| 移动端解码 | 优先系统解码器，FFmpeg 仅 demux/remux |
| 转码 | 加密时可选转为 web 原生格式，强制二次确认（不可逆） |
| `BlockSource` | 同步 trait（偏离文档草案的 async，理由见上） |

## 已知偏差与坑

- `chunk_size` 范围校验**只能**在新建文件时做，不能放解析路径——测试向量 v3/v4 用 4096/2048 字节块，格式上合法。
- Argon2 耗时强依赖设备（本机 release 75.4 ms，文档记 180 ms），**不得**把任何具体耗时当常量写入代码。
- 文档中"扫描需 2.5 小时"对应 `10,000 文件 × 5 密码 × 180 ms`；单密码口径为 30 分钟。两种口径下两级 KDF 都是可行性前提。
- `bench` 的"扫描 N 文件"外推**只含密钥运算**，不含文件 I/O。实测 300 文件约 95 ms 说明 I/O 才是主导项，不要把这个外推当端到端耗时。
- `scan` 目前会遍历目录两遍（一遍探 vault 配置、一遍解锁）。两遍都只读文件头不读载荷，相比一次 Argon2 可忽略；但网络文件系统上目录遍历本身可能很慢，届时应改为一次遍历、边收集边惰性派生。
- `CipherId::ChaCha20Poly1305` 是 **12 字节 nonce 的 RFC 8439 版本，不是 XChaCha20**。`--cipher xchacha20` 当前实际落到前者，`doctor` 会如实报告这一点。
- 测试代码用 `#![cfg_attr(test, allow(...))]` 在 crate 根统一放宽 `unwrap_used` / `indexing_slicing`；**库代码不放宽**——库处理不可信输入，任何 panic 路径都是拒绝服务缺陷。
- `cargo search` 在当前网络下必然超时（镜像的 `source.crates-io.replace-with` 只作用于依赖解析，不影响 registry API）。查版本走 rsproxy sparse 索引 `https://rsproxy.cn/index/<按名长分层路径>`，不影响正常构建。
- PowerShell 内联嵌套引号构造 Rust 代码字符串极易 ParserError。改文件请用 Write/Edit 工具，不要用 shell 拼字符串。

### 校验外部命令产物：只看长度会自证成功

排查尾部 moov 抽帧时，诊断脚本用 `> out 2>&1` 把 stdout 与 stderr 混进同一文件，
再用"长度 > 100"判定成功。结果 844 字节的 FFmpeg **错误文本**被当成 WebP 图片，
据此得出「尾部 moov 也能抽帧」的错误结论，并按这个错误结论改了实现——
绕了两轮才发现前 16 字节是 `[in#0/mov,...`。

由此定下两条硬规矩：

1. **诊断时 stdout 与 stderr 必须分开重定向**，绝不用 `2>&1` 混流；
2. **校验产物必须看内容特征**：验图片先验 WebP(`RIFF....WEBP`)/JPEG(`FFD8FF`)/PNG 魔数，
   再做一次真实解码。退出码为 0 不代表产物有效，非零也不代表没产出。

同理，FFmpeg 的退出码不可单独作为判据——需要 `stdout 非空` + `内容可解码` 双重确认。

### ffprobe 对 MKV 与 WebM 返回相同的 format_name

两者都是 `matroska,webm`（WebM 是 Matroska 子集、共用 demuxer），
但 `<video>` 支持 WebM、不支持 MKV。**只靠容器名判断必然把所有 MKV 误判为 P1**。
`tier::container_natively_supported()` 改为按内容判断：只有视频全在 vp8/vp9/av1
且音频全在 vorbis/opus 才算真 WebM。已有 `mkv_and_webm_share_format_name_but_differ_in_tier` 锁定。

另：`format_name` 是**逗号分隔多值**（MP4 返回 `mov,mp4,m4a,3gp,3g2,mj2`），不能当单值比较。
数值字段类型也不统一——`duration`/`size`/`bit_rate` 是字符串，`channels`/`width`/`index` 是数字，
还可能是 `"N/A"`，故用自定义 `LooseNumber` 容忍四态。

### `payload::read_range` 的 `fetch_ct` 偏移语义（最易踩）

回调收到的偏移是**相对载荷起点**的，不是文件绝对偏移——`read_range`
内部已经减掉了 `header_len`。数据源若是整个文件，必须自己加回：

```rust
let header_len = u64::from(opened.header.header_len);
read_range(&opened.header, opened.payload_key(), None, off, len,
    |payload_off, l| {
        let abs = payload_off + header_len;   // ← 漏掉这步就出错
        Ok(bytes[abs as usize..(abs + l) as usize].to_vec())
    })
```

漏掉会取到偏移 `header_len` 的字节，报 `ChunkAuthFailed`。**错误信息
指向「数据损坏或被篡改」，极易误判成文件坏了或密码不对**，实际是偏移
用错。实现 spike 时踩过一次，已在 `payload.rs` 补测试固化——其中一项
专门断言错误用法必然触发 `ChunkAuthFailed`。

之所以此前没被发现：往返测试都走 `decrypt_all`，它内部自己算偏移，
完全绕过了 `fetch_ct` 契约。这类「只有外部调用方才会踩」的接口坑，
自测同源的用例覆盖不到，必须有独立调用方（如 spike）才能暴露。

对只存载荷的数据源（分片、远程对象存储）而言，相对偏移正是它需要的
形式，所以这个设计本身是对的，只是文档要写得更醒目。
