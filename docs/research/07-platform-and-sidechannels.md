# 07 · 平台适配与旁路防护

## 1. 平台能力矩阵

| 能力 | Windows | macOS | Linux | Android | iOS |
|---|:---:|:---:|:---:|:---:|:---:|
| **文件访问** ||||||
| 任意路径读写 | ✅ | ✅ | ✅ | ⚠️ SAF | ❌ 仅书签 |
| 全盘扫描 | ✅ | ⚠️ 需 FDA | ✅ | ⚠️ 需特权 | ❌ **不可能** |
| 持久化目录授权 | ✅ | ✅ 书签 | ✅ | ✅ tree URI | ✅ 书签 |
| **播放** ||||||
| P1 自定义协议 + Range | ✅ | ✅ | ✅ | ✅ | ⚠️ **需验证** |
| P2 MSE | ✅ | ✅ | ✅ | ✅ | ⚠️ iOS 17.1+ |
| P3 全解码 | ✅ | ✅ | ✅ | ❌ 不提供 | ❌ 不提供 |
| 系统硬件解码 | 🔜 未来 | 🔜 未来 | 🔜 未来 | ✅ 优先 | ✅ 优先 |
| **外部打开** ||||||
| 不落盘 | ❌ | ❌ | ❌ | ✅ **可以** | ❌ |
| **安全** ||||||
| 生物识别 | ✅ Hello | ✅ Touch/Face ID | ❌ | ✅ | ✅ |
| 硬件密钥库 | ✅ TPM | ✅ Secure Enclave | ⚠️ 无统一方案 | ✅ TEE/StrongBox | ✅ SE |
| 进程沙箱（FFmpeg） | ✅ AppContainer | ✅ sandbox_init | ✅ seccomp | ⚠️ 已在沙箱内 | ⚠️ 同 |
| 防截屏 | ⚠️ 部分 | ⚠️ 部分 | ❌ | ✅ FLAG_SECURE | ⚠️ 需自行遮挡 |
| **网络** ||||||
| mDNS 发现 | ✅ | ✅ | ⚠️ 需 Avahi | ✅ NSD | ⚠️ 需授权 |

---

## 2. 各平台文件访问

### 2.1 Windows

- 任意路径访问，无特殊限制
- ⚠️ **长路径**：默认 260 字符限制。需在 manifest 中声明 `longPathAware`，且要求 Windows 10 1607+ 并启用注册表项。否则深层目录 + base32 加密目录名（1.6× 膨胀）容易触顶
- ⚠️ **保留文件名**：`CON`、`PRN`、`AUX`、`NUL`、`COM1-9`、`LPT1-9` 不可用作文件名（即使带扩展名）
- 非法字符：`\ / : * ? " < > |`

### 2.2 macOS

- 沙箱应用（App Store 分发）需要 security-scoped bookmark
- **完全磁盘访问（Full Disk Access）**：扫描 `~/Documents`、`~/Desktop` 等受保护目录需要用户在系统设置中手动授予
- ⚠️ APFS **默认大小写不敏感** → 这是必须用 base32 而非 base64 编码文件名的直接原因
- 公证（Notarization）：分发需要 Apple Developer 账号

### 2.3 Linux

- 最自由，无特殊限制
- 打包格式建议：AppImage（免安装）+ Flatpak（沙箱，需声明 `--filesystem=home`）+ deb/rpm
- ⚠️ Flatpak 沙箱下的文件访问受限，需正确声明 portal 权限

### 2.4 Android

Tauri 的 fs 插件**默认只能访问应用私有目录**，访问外部存储必须走 SAF：

```
用户选择目录 (ACTION_OPEN_DOCUMENT_TREE)
  → 获得 tree URI
  → takePersistableUriPermission()  持久化授权
  → 后续通过 DocumentFile / ContentResolver 访问
```

⚠️ **`MANAGE_EXTERNAL_STORAGE`（全盘访问）在 Google Play 审核极严**——非文件管理器类目基本会被拒。因此 Android 上也是"授权目录扫描"，不是全盘扫描。

现成插件：`tauri-plugin-scoped-storage` 封装了 SAF 持久化 tree URI（iOS 侧封装 `UIDocumentPickerViewController` + security-scoped bookmark）。

**其他 Android 注意点**：
- Google Play 自 2025 年 11 月起要求支持 **16 KB 内存页** → 影响 FFmpeg 等原生库的构建配置
- 后台任务限制：加密大文件时应用可能被系统冻结，需用 Foreground Service + 通知

### 2.5 iOS（降级方案，决策 D-06）

**系统层面不允许全盘扫描。** iOS 定位为：

```
✅ 导入式保险箱：UIDocumentPicker 选文件导入到应用沙盒
✅ 局域网客户端：连接其他设备浏览、播放
✅ 授权目录：用户选定的目录 + security-scoped bookmark 持久化
❌ 全盘扫描：不可能
```

Tauri fs 插件会自动管理 security-scoped 资源的访问（`startAccessingSecurityScopedResource` / `stopAccessing`）。

**其他 iOS 注意点**：
- 文件 App 可见性：需在 Info.plist 设置 `UIFileSharingEnabled` + `LSSupportsOpeningDocumentsInPlace`
- 后台挂起：加密任务需支持中断续传
- 出口合规：需声明 `ITSAppUsesNonExemptEncryption`（见 [10](10-licensing-and-patents.md)）

---

## 3. 🔴 旁路泄露清单

> **这是本文档最重要的一节。** 加密算法再正确，一个旁路泄露就能让整个设计失效。

### 3.1 完整清单

| # | 泄露点 | 风险 | 对策 | 优先级 |
|---|---|---|---|:---:|
| **L1** | **WebView HTTP 缓存** | 解密后的媒体数据被 WebView 落盘 | `Cache-Control: no-store` + 退出时清理缓存目录 | 🔴 |
| **L2** | **临时解密文件** | 外部应用打开时产生的明文文件 | 受控目录 + 生命周期管理 + 优先内存盘 | 🔴 |
| **L3** | **系统缩略图缓存** | OS 为临时明文文件生成缩略图并缓存 | Windows: 临时目录设 `desktop.ini` 禁用；macOS: 避免放 `~/Pictures`；Linux: 避免 `~/.cache/thumbnails` | 🔴 |
| **L4** | **最近打开记录** | OS 记录用户打开过的文件名 | Windows: 不用 `SHAddToRecentDocs`；macOS: `NSDocumentController` 禁用；提示用户手动清理 | 🟡 |
| **L5** | **系统搜索索引** | Spotlight / Windows Search 索引临时明文文件的**内容** | 临时目录标记为不索引（Windows: `FILE_ATTRIBUTE_NOT_CONTENT_INDEXED`；macOS: `.metadata_never_index` 标记文件） | 🔴 |
| **L6** | **剪贴板** | 用户复制的明文内容残留 | 密码框禁用复制；敏感内容复制后定时清空 | 🟡 |
| **L7** | **移动端任务切换器截图** | 系统截屏保存已解密内容 | iOS: `willResignActive` 覆盖模糊层；Android: `FLAG_SECURE` | 🔴 |
| **L8** | **崩溃转储 / core dump** | 内存中的密钥被写入磁盘 | 禁用 core dump（`setrlimit(RLIMIT_CORE, 0)`）；Windows 禁用 WER 转储 | 🟡 |
| **L9** | **交换分区 / 休眠文件** | 内存换页把密钥写入磁盘 | 威胁模型不要求；建议用户启用全盘加密 | 🟢 |
| **L10** | **日志文件** | 误把文件名、路径、密钥写入日志 | 日志脱敏；`Debug` impl 手动实现，密钥类型不输出内容 | 🔴 |
| **L11** | **WebView DevTools** | 生产构建中残留调试端口 | Release 构建禁用 devtools | 🔴 |
| **L12** | **进程命令行参数** | CLI 传密码时 `ps` 可见 | CLI 禁止 `--password` 明文参数，只用 stdin / 环境变量 / 交互输入 | 🔴 |
| **L13** | **备份系统** | Time Machine / 文件历史备份了临时明文 | 临时目录标记排除备份（macOS: `NSURLIsExcludedFromBackupKey`） | 🟡 |
| **L14** | **云盘同步** | 临时目录恰好在 OneDrive/iCloud 同步范围内 | 临时目录选择时避开已知同步路径 | 🟡 |
| **L15** | **MSE 路径的 JS 堆** | P2/P3 下明文经过 JS 内存 | 用完置空 ArrayBuffer；本威胁模型下可接受 | 🟢 |
| **L16** | **文件系统元数据** | 加密文件的 mtime、大小暴露活动模式 | 可选归一化（默认关闭） | 🟢 |

### 3.2 L1 的具体处理（各平台缓存目录）

| 平台 | 清理方式 |
|---|---|
| Windows (WebView2) | `%LOCALAPPDATA%\{app}\EBWebView\Default\Cache` + `ICoreWebView2Profile::ClearBrowsingDataAsync` |
| macOS/iOS (WKWebView) | `WKWebsiteDataStore.default().removeData(ofTypes:modifiedSince:)` |
| Linux (WebKitGTK) | `WebKitWebsiteDataManager.clear()` |
| Android | `WebView.clearCache(true)` + `WebStorage.getInstance().deleteAllData()` |

**清理时机**：应用退出、会话锁定、启动时（清理上次残留）。

### 3.3 L12 的具体处理

```bash
# ❌ 绝对禁止 —— ps aux 可见，shell history 也会记录
omy decrypt --password "my-secret" file.omy

# ✅ 交互式输入（默认）
omy decrypt file.omy
Password: ********

# ✅ 从 stdin 读取（脚本场景）
echo "$PASSWORD" | omy decrypt --password-stdin file.omy

# ✅ 从文件读取（权限 0600）
omy decrypt --password-file ~/.omy/pw file.omy
```

CLI **必须**在检测到 `--password` 参数时**直接报错退出**，并说明替代方式。

---

## 4. FFmpeg 沙箱隔离

### 4.1 各平台实现

| 平台 | 机制 | 要点 |
|---|---|---|
| **Linux** | seccomp-bpf + namespaces | 白名单系统调用；`CLONE_NEWNET` 断网；`CLONE_NEWNS` 隔离挂载 |
| **macOS** | `sandbox_init` (Seatbelt) | 虽已废弃但仍可用；或用 XPC service + entitlements |
| **Windows** | AppContainer / Job Object | 低完整性级别；Job Object 限制 CPU/内存 |
| **Android** | 已在应用沙箱内 | 额外：独立进程 + `android:process` + 不声明网络权限 |
| **iOS** | 已在应用沙箱内 | iOS 不允许 fork 子进程，需用线程 + 严格输入校验 |

⚠️ **iOS 是例外**：不能创建子进程。FFmpeg 只能在同进程内运行 —— 这意味着 iOS 上 FFmpeg 的崩溃会导致整个应用崩溃，且攻击面无法隔离。**这是 iOS 上应尽量只用 P1 路径的又一理由。**

### 4.2 资源限制

```rust
struct SandboxLimits {
    max_cpu_seconds: u32,      // 默认 300（转码任务需更长）
    max_memory_mb: u32,        // 默认 512
    max_output_bytes: u64,     // 防 zip bomb 式输出膨胀
    max_wall_clock: Duration,  // 硬超时，超时强杀
}
```

### 4.3 通信

```
主进程 ──stdin(明文字节)──▶ FFmpeg 沙箱进程 ──stdout(处理结果)──▶ 主进程
```

- **不传文件路径**，只传字节流 → 子进程即使被攻陷也无法访问文件系统
- **不传密钥**
- 子进程无网络权限

---

## 5. 内存与密钥卫生

### 5.1 Rust 层面

```rust
use zeroize::{Zeroize, Zeroizing};

// 所有密钥类型
type Key = Zeroizing<[u8; 32]>;

// 手动实现 Debug，防止密钥被日志打印
struct Fek([u8; 32]);
impl std::fmt::Debug for Fek {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Fek(<redacted>)")
    }
}
```

**检查清单**：
- [ ] 所有密钥类型用 `Zeroizing` 包装
- [ ] 所有密钥类型手动实现 `Debug`（不输出内容）
- [ ] 明文缓冲区在释放前 zeroize
- [ ] 避免 `String`/`Vec` 的隐式重分配导致旧内存残留（用 `with_capacity` 预分配）
- [ ] 禁用 core dump

### 5.2 不做的事

| 措施 | 不做的原因 |
|---|---|
| `mlock` | 威胁模型不要求；跨平台行为不一致；需要提升权限；Linux 有 `RLIMIT_MEMLOCK` 限制 |
| 密钥分散存储 / 白盒密码 | 防的是内存取证（N2/N3，已排除） |
| 常量时间的全部操作 | AEAD 库已保证关键路径；应用层不涉及秘密依赖分支 |

---

## 6. WebView 安全配置

### 6.1 CSP（内容安全策略）

```
default-src 'none';
script-src 'self';
style-src 'self' 'unsafe-inline';
img-src omy: data: blob:;
media-src omy: blob:;
font-src 'self';
connect-src 'self' ipc: http://ipc.localhost;
object-src 'none';
frame-src 'none';
base-uri 'none';
form-action 'none';
```

⚠️ `style-src 'unsafe-inline'` 是多数 CSS-in-JS 方案的现实妥协；若能避免则移除。

### 6.2 SVG 处理（决策 D-13）

见 [04-媒体播放 §10.3](04-media-playback.md)。核心：**只用 `<img>` 渲染，绝不 inline**。

### 6.3 Tauri 权限最小化

```json
{
  "permissions": [
    "core:default",
    { "identifier": "fs:scope", "allow": [] }
  ]
}
```

**关键原则**：前端**不应该有任何 fs 权限**。所有文件操作通过自定义 command 走 Rust 侧，由 Rust 做路径校验。

理由：即使前端被 XSS 完全攻陷，攻击者也无法突破 Rust 侧预设的访问边界。

### 6.4 IPC 边界

```rust
// ❌ 危险：前端传路径
#[tauri::command]
fn read_file(path: String) -> Vec<u8> { ... }

// ✅ 安全：前端只持有不透明句柄
#[tauri::command]
fn read_entry(handle: EntryHandle, offset: u64, len: u32) -> Vec<u8> {
    // handle 由 Rust 侧在扫描时分配，映射到真实路径
    // 前端无法构造任意 handle
}
```

---

## 7. 移动端特殊处理

### 7.1 后台中断（决策：可续传）

```
加密大文件时应用被切后台
  → iOS: 进程被挂起（beginBackgroundTask 只能争取约 30 秒）
  → Android: 可用 Foreground Service 延续

方案：
  1. 分段提交：每完成 N 个块，把进度写入 .omy.progress
  2. 下次启动检测未完成任务
  3. 提示"上次有 1 个文件未加密完成，继续 / 放弃？"
  4. 放弃时清理 .tmp 和 .progress
```

### 7.2 防截屏（L7）

| 平台 | 实现 | 副作用 |
|---|---|---|
| Android | `window.addFlags(FLAG_SECURE)` | 同时阻止用户主动截屏 → **必须做成可配置** |
| iOS | 监听 `willResignActive`，覆盖模糊视图；`didBecomeActive` 时移除 | 无法阻止用户主动截屏（系统限制） |

### 7.3 资源限制

| 参数 | 桌面 | 移动端 |
|---|---|---|
| Argon2 档位上限 | sensitive (1 GiB) | **moderate (256 MiB)**，且需内存检查 |
| chunk_size 上限 | 16 MiB | **1 MiB** |
| LRU 块缓存 | 8 块 | 3 块 |
| 并发扫描线程 | CPU 核数 | 2 |
| FFmpeg | 完整 | 仅 demux/remux |

**打开高参数文件的降级提示**：

```
⚠️ 无法在此设备打开

   此文件使用了高强度加密参数（需要 1 GiB 内存）。
   当前设备可用内存：412 MB

   请在桌面端打开，或通过局域网连接你的电脑。
```

---

## 8. 错误处理与损坏恢复

### 8.1 错误分类

| 类别 | 示例 | 用户提示 | 可恢复 |
|---|---|---|:---:|
| **密码错误** | 所有 slot 解包失败 | "密码不正确，或此文件不属于当前密码" | ✅ |
| **格式不识别** | magic 不匹配 | "这不是 omy 文件" | — |
| **版本过新** | `version_major > 1` | "此文件由更新版本创建，请升级应用" | ✅ 升级 |
| **未知 critical TLV** | 不认识的 CRITICAL 字段 | "此文件使用了当前版本不支持的功能" | ✅ 升级 |
| **header 损坏** | MAC 校验失败 | "文件头已损坏或被篡改" | ⚠️ 见 §8.2 |
| **载荷损坏** | 某块 AEAD 失败 | "文件第 N 部分已损坏" | ⚠️ 部分 |
| **缺少分片** | 序号不连续 | "缺少第 [3,5] 片" | ✅ 找回分片 |
| **资源不足** | Argon2 内存不够 | 见 §7.3 | ✅ 换设备 |

### 8.2 损坏恢复能力

**分块 AEAD 的优势**：损坏是**局部的**。

| 损坏位置 | 影响 |
|---|---|
| 某个载荷块 | 只有该块（默认 256 KiB）不可读，其余正常 |
| header | ⚠️ 整个文件不可读 —— **除非有分片冗余 header** |
| 某个分片 | 只影响该片覆盖的区间 |
| 压缩索引表 | ⚠️ 整个文件不可读（CRITICAL TLV） |

**恢复手段**：
1. **分片冗余 header** —— 主 header 损坏时，从任一分片的冗余副本恢复
2. **部分读取模式** —— CLI 提供 `--ignore-errors`，跳过损坏块，输出可读部分并报告
3. **内容哈希校验** —— 解密后比对 `TLV_CONTENT_HASH`，确认完整性

### 8.3 健壮性要求（处理不可信输入）

所有从文件读取的长度/数量字段**必须**校验上界并使用 checked 运算：

```rust
const MAX_HEADER_LEN: u32 = 64 * 1024 * 1024;
const MAX_TLV_LEN: u32 = 32 * 1024 * 1024;
const MAX_TLV_ENTRIES: usize = 4096;
const MAX_INDEX_ENTRIES: u32 = 16 * 1024 * 1024;
const MAX_CHUNK_SIZE: u32 = 64 * 1024 * 1024;
const MIN_CHUNK_SIZE: u32 = 4096;
const MAX_FOLDER_ENTRIES: usize = 10_000_000;

// 所有偏移计算
let end = offset.checked_add(len).ok_or(Error::Overflow)?;
```

**必须做 fuzzing**：`cargo-fuzz` 针对 header 解析、TLV 解析、索引表解析。这是处理不可信输入的代码的标配。

---

## 9. 打包与分发

| 平台 | 格式 | 签名要求 |
|---|---|---|
| Windows | MSI / NSIS / 便携版 zip | 代码签名证书（否则 SmartScreen 警告） |
| macOS | .dmg / .pkg | Apple Developer 签名 + **公证（Notarization）** |
| Linux | AppImage / Flatpak / deb / rpm | 可选 GPG 签名 |
| Android | APK / AAB | 签名密钥；Play 上架需 16 KB 页支持 |
| iOS | IPA | Apple Developer 账号；出口合规声明 |

### 9.1 可复现构建（开源项目应做）

加密软件的可信度很大程度依赖"用户能验证发布的二进制确实来自公开源码"：

- 固定 Rust 工具链版本（`rust-toolchain.toml`）
- `Cargo.lock` 提交到仓库
- 记录构建环境（容器镜像哈希）
- 发布构建日志和产物哈希
- 理想情况：多方独立构建产出相同哈希
