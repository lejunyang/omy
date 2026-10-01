# 第三方密码管理器集成

> 状态：首期已实现并完成桌面实测，2026-10-01。首个桌面 provider 为
> KeePassXC，Android provider 走系统 Credential Manager；文件格式与密码
> slot 不变。Android 真机与跨端元数据仍待验证，见 5.1。

## 1. 目标

omy 要支持一把由密码管理器保存并随 KDBX 同步的高熵密钥：

- 加密时可以选择已经存在的密钥；
- 没有合适密钥时，由 omy 生成并保存；
- 解密时唤起密码管理器选择，不要求用户抄写或记忆；
- KeePassXC 与 KeePassDX 同步同一份 KDBX 后，两端使用同一秘密；
- 密码管理器不可用时，手工密码与恢复码仍能独立工作。

首期保存的是**普通密码条目里的 256 bit 随机秘密**，不是 passkey。随机秘密
继续走现有密码路径：

```text
密码管理器中的随机秘密
        │
        ▼
Argon2id(secret, vault_salt)
        │
        ▼
现有 KEK → key slot → FEK
```

这样不修改 `.omy` 格式，也不让 KeePassXC 成为打开文件的必要依赖。拿到秘密的
用户仍可通过 GUI 密码框或 CLI 的既有密码通道解锁。

## 2. 为什么 crate 叫 `omy-password-manager`

实现不能叫 `omy-keepassxc`：KeePassXC 只是第一个 provider，Android 使用的是
系统 Credential Manager，未来还可能接入 Apple AuthenticationServices、
1Password、Bitwarden 或其它实现。

```text
omy-password-manager
├── 公共 provider trait 与领域类型
├── KeePassXC-Browser 桌面 provider
├── Android Credential Manager 桥接契约
└── 后续第三方 provider
```

它与 `omy-secret` 的职责不同：

| crate | 解决的问题 |
|---|---|
| `omy-secret` | 用本机 OS 密钥库/TPM 保护 omy 自己落盘的秘密 |
| `omy-password-manager` | 经用户授权，从外部密码管理器选择、创建或更新秘密 |

两者不能合并。前者是静默的本机保管层，后者会调起外部应用并发生用户选择。

## 3. 凭据模型

### 3.1 同步密钥

同步密钥由 Rust 后端的 CSPRNG 生成 32 字节，再编码成：

```text
omy1_<43 字符无填充 base64url>
```

前缀只用于识别版本和防止用户误选普通网站密码，不提供安全性。解锁时仍允许
任意字符串，以兼容已有密码；只有“生成新同步密钥”保证上述格式。

KDBX 条目建议为：

| 字段 | 值 |
|---|---|
| Group | `omy` |
| Title | provider 能设置时使用 `omy 同步密钥` |
| UserName | 用户给出的名称，如“个人主密钥” |
| Password | `omy1_...` |
| URL | `https://credentials.omy.app/` |

URL 只充当 KeePassXC 查询命名空间，omy 不会访问它。正式发布前必须确认该域名
长期受项目控制；不能使用他人的域名，也不能使用 `localhost` 作为长期身份。

### 3.2 一把还是每个 vault 一把

默认是一把命名同步密钥覆盖多个 vault。现有 KDF 已用 `vault_salt` 隔离：

```text
同一个 secret + vault A salt → KEK A
同一个 secret + vault B salt → KEK B
```

用户可以另建“工作资料”“家庭共享”等密钥缩小泄露范围。文件头不写密码管理器
条目 UUID 或名称：公开标识会让观察者把不同文件关联到同一身份，也会破坏现有
可否认模式。选错密钥时只报告“所选密钥不能打开这里”。

### 3.3 生命周期

- 创建新密钥：**先保存并回读验证，再加密**；反过来会产生无钥匙密文。
- 更新/轮换：先添加新 slot 并验证，再移除旧 slot。
- 删除 KDBX 条目不自动改 `.omy`：同一秘密可能仍被其它文件使用。
- KDBX 写入成功不等于云同步完成，恢复码仍是必要后备。

## 4. KeePassXC 桌面 provider

### 4.1 传输

不加载浏览器扩展，也不直接读 KDBX。omy 启动 KeePassXC 自带的
`keepassxc-proxy`，使用 Native Messaging 帧：

```text
u32 little-endian JSON 长度 || UTF-8 JSON
```

proxy 经 Windows Named Pipe 或 Unix Domain Socket 转发到正在运行的
KeePassXC。omy 不直连内部 socket：其位置随 Windows、macOS、Linux、Snap、
Flatpak 和 AppImage 而变，proxy 正是官方用来屏蔽这层差异的组件。

不使用可选的 `ws://localhost:7580`，避免额外本机监听端口和 WebSocket 攻击面。

### 4.2 会话加密与关联

每次连接先生成临时 NaCl box 密钥对，调用 `change-public-keys` 交换公钥。后续
敏感 JSON 使用 X25519 + XSalsa20-Poly1305 加密，每个请求使用新的 24 字节
nonce，响应必须携带加一后的 nonce。

首次对一个数据库调用 `associate` 时，KeePassXC 必须弹出确认窗口。用户给连接
命名（建议 `omy-<设备名>`）后，双方保存 association id/key。以后用
`test-associate` 恢复授权。

association key 实际是 bearer credential：知道它的本机进程可以恢复关联，
所以 omy 必须用 `omy-secret` 保存，不得明文进入配置、日志或崩溃报告。每台设备
单独关联，不随 KDBX 或 omy 配置同步。

### 4.3 首期使用的动作

| 动作 | 用途 |
|---|---|
| `change-public-keys` | 建立临时加密通道 |
| `get-databasehash` | 识别当前打开的 KDBX 与版本 |
| `associate` / `test-associate` | 首次授权与恢复关联 |
| `get-logins` | 按固定 URL 查询同步密钥 |
| `set-login` | 创建或更新同步密钥 |
| `get-database-groups` | 找到目标分组 |
| `create-new-group` | 经用户确认创建 `omy` 分组 |

不使用 `get-database-entries`：它会扩大到整库枚举，还要求用户打开一个额外的
高风险设置。也不自动使用 `delete-entry`：删除密码管理器条目可能同时断掉多份
文件的访问权。

### 4.4 锁定与选择

数据库锁定时，请求可带 `triggerUnlock=true` 让 KeePassXC 前置并显示自己的
解锁界面。omy 不接收 KDBX 主密码；解锁完成后收到状态事件并重试原操作。

`get-logins` 的第一次访问会让 KeePassXC 显示条目授权窗口。若用户已经记住
权限，接口可能一次返回多个条目及其密码。为避免把一批秘密送入 WebView：

1. Rust 后端解密响应并保留候选项；
2. 前端只得到名称、用户名、分组与随机临时 handle；
3. 用户选择 handle 后，Rust 直接调用现有加密/解锁入口；
4. 全部候选密码立即清零；
5. 超时、取消、锁定和切换数据库时也清零。

### 4.5 兼容与安装发现

首期自动探测常见原生安装路径，同时允许用户选择 `keepassxc-proxy`：

- Windows 安装版与便携版；
- macOS `KeePassXC.app/Contents/MacOS/keepassxc-proxy`；
- Linux `PATH`、常见 `/usr/bin` 与 `/usr/local/bin`。

Snap/Flatpak 的沙箱桥接另行实测，探测失败时不猜路径、不降级为读取 KDBX。

KeePassXC-Browser 是长期存在的实现协议，但不是承诺兼容的第三方 SDK。客户端
必须读取 `change-public-keys` 响应里的版本，只启用实测过的动作，并始终保留
手工密码/Auto-Type 回退。

## 5. Android provider

Android 14+ 通过 Jetpack Credential Manager：

```text
选择：GetCredentialRequest(GetPasswordOption)
创建：CreatePasswordRequest(label, generated_secret)
```

KeePassDX 4.5 已同时实现密码读取、密码创建与 Passkey PRF。Android 7–13 保留
WebView Autofill / KeePassDX Autofill / Magikeyboard 回退。omy 当前最低 API 24，
所以运行期必须判断能力，不得把 Android 14 API 放进无保护路径。

密码应从 Kotlin provider 桥传给 Rust 后端并直接完成业务操作，不返回 Vue。
当前 WebView 已开启 autofill，密码输入框也已有 `current-password` / `new-password`
标记，但这只是兼容入口，不代替显式 Credential Manager 按钮。

### 5.1 KDBX 跨端元数据

秘密本身使用标准 Password 字段，可以无损同步。自动匹配元数据不同：

- KeePassXC 按 `URL=https://credentials.omy.app/` 查询；
- KeePassDX 为原生应用记录 `AndroidApp=org.omy.app` 与发布签名指纹。

对 KeePassDX 4.5.5 与 KeePassXC Browser 源码的核验结论是：**普通客户端无法
在任意一端通过现有标准 API 自动把两类元数据同时写入同一条记录**。

| 创建入口 | API 能提供的字段 | 缺少的字段 |
|---|---|---|
| KeePassXC-Browser `set-login` | URL、用户名、密码、分组 | `AndroidApp` 与签名；协议不接收任意自定义字段 |
| Android `CreatePasswordRequest` | 用户名、密码、由系统认证的调用方包名/签名 | Web URL；普通应用不能自报 Web origin |

Android 的 `origin` 参数不是通用扩展点。只有持有系统级
`CREDENTIAL_MANAGER_SET_ORIGIN` 权限并被密码管理器列入特权调用方名单的
浏览器才能代表网页设置它；omy 冒充 Web origin 会破坏 Credential Manager
用来防止钓鱼应用读取网站密码的边界，也会被系统拒绝。另一方面，当前
KeePassXC-Browser 的 `set-login` 只消费 URL/login/password/group，无法写
KeePassDX 使用的自定义 `AndroidApp` 字段。

因此首期实现保留平台原生且安全的路径，并明确要求一次性补关联：

1. **Android 创建 → 桌面使用**：KeePassDX 会自动写包名和签名；同步前在同一
   条目补上 `URL=https://credentials.omy.app/`，桌面即可查询。
2. **桌面创建 → Android 使用**：URL 已存在；第一次在 Android 上通过普通密码
   输入框的 KeePassDX Autofill 手动选中这条记录，并允许 KeePassDX 把当前应用
   关联写回该条目。之后 Android 14+ 的 Credential Manager 才能直接命中。
3. 不能在 Android 的“生成并保存”流程里选择一个已有桌面条目来冒充关联：
   `CreatePasswordRequest` 携带的是刚生成的新秘密，更新旧条目会覆盖原密码，
   让旧文件失去原来的解锁材料。

这不满足“任意一端创建时零人工步骤写齐两端元数据”的理想目标；它是两个上游
API 的能力缺口，不应由 omy 伪造来源或直接改用户 KDBX 来绕过。后续可向
KeePassXC-Browser 提议受确认的自定义字段写入，并向 KeePassDX 提议在创建界面
附加受用户确认的 Web URL；只有两边至少一边提供这种能力后，才能安全自动化。

Android 包名与发布签名必须长期稳定。debug 与 release 签名被视为不同应用，
测试报告要同时打印包名和签名指纹，避免把关联失败误判成密钥错误。

## 6. Passkey PRF 的后续位置

KeePassXC 当前的 `passkeys-register` / `passkeys-get` 只支持注册和签名，未实现
WebAuthn `prf`；普通签名绝不能作为 KEK。KeePassDX 4.5 已将 PRF secret 放入
受保护的 `KPEX_PASSKEY_PRF` 字段，但 KeePassXC 当前不会消费或返回该字段。

所以首期明确不做 passkey。未来 KeePassXC 与其它 provider 都能返回 PRF 后，
再增加独立的 `Passkey` credential/slot：

```text
root = WebAuthn-PRF(passkey, SHA256("omy/v1/passkey-root"))
KEK  = HKDF-SHA256(root, salt=vault_salt, info="omy/v1/passkey-kek")
```

Passkey 与设备密钥都属于便利凭据，不能成为唯一 slot。缺少 PRF 时必须明确报
不支持，不能退回签名、软件随机密钥或本机设备密钥冒充成功。

## 7. UI

加密对话框：

```text
密钥来源

● 密码管理器同步密钥
  [选择已有密钥]
  [生成并保存新密钥]

○ 手工输入密码
```

解锁对话框：

```text
[从密码管理器选择]
[使用 Windows Hello]   （已挂设备密钥时）

或者输入密码
```

设置页显示 provider 状态、当前数据库、关联名、重连和忘记关联。错误必须区分：

- 未安装 provider；
- provider 未运行；
- 集成未开启；
- 数据库已锁定；
- 未关联/关联被撤销；
- 用户取消；
- 没有匹配条目；
- 协议或版本不兼容；
- 条目已保存但后续加密失败。

## 8. 安全约束

1. 密码、关联 key、NaCl 私钥不得写日志或进入 `Debug`。
2. Native Messaging 长度设硬上限，分配内存前拒绝超长帧。
3. 响应 action、nonce、数据库 hash 都要核对，拒绝重放和串响应。
4. proxy 必须按绝对路径直接启动，不经 shell，不接受拼接参数。
5. 密码候选只在 Rust 内存短暂存在，锁定/取消/超时全部清零。
6. 只查询固定 omy URL，不请求整库枚举，不要求“总是允许所有访问”。
7. 新密钥由 omy CSPRNG 生成；KeePassXC 密码生成器只作为可选人工入口。
8. 支持回读的 provider（当前为 KeePassXC）保存后必须回读同一 UUID 和秘密再
   开始加密；Android 标准创建响应不返回 provider 条目，也没有安全的无交互
   回读，所以真机测试必须验证保存结果，界面不得声称已经回读。
9. 任何 provider 失败都不静默回退到弱存储。
10. 示例 KDBX 只含测试数据，文件名和同目录说明必须明确密码为 `123456`，
    产品代码与文档不得把它当安全示例。

## 9. 验证计划

### 9.1 协议单测

- 分片读取长度头与 JSON，不这样会把正常半包当损坏；
- 超长帧在分配前拒绝，不这样会被本机恶意进程耗尽内存；
- nonce 不匹配、MAC 篡改、action 串台必须失败；
- 数据库切换后不能沿用错误关联；
- `get-logins` 解析时敏感字段不进入 `Debug`；
- 创建、更新、无匹配、取消与锁定错误分别映射。

### 9.2 KeePassXC 真实验证

用仓库内测试 KDBX（密码 `123456`，只含假秘密）验证：

1. 未启用 Browser Integration 时明确失败；
2. 首次关联必须出现 KeePassXC 确认；
3. 数据库锁定时能拉起解锁；
4. 查询只返回 omy URL 条目；
5. 生成后能回读相同的 256-bit 秘密；
6. 该秘密能加密并解密真实 `.omy` 文件；
7. 取消授权、切换数据库、关闭 KeePassXC 都清除候选；
8. proxy 退出后不留下子进程。

2026-10-01 在 Linux、KeePassXC 2.7.4 与仓库 fixture 上完成了 2、4、5、6、8：
真实关联和条目授权成功，固定秘密经过 `omy-core` 加密/解密往返，生成条目也能
回读 UUID。锁定、撤销授权与切库仍应在桌面 GUI 端到端测试中继续覆盖。

### 9.3 Android 真实验证

Android 验证必须走真实界面和 KeePassDX，不用 mock 代替最终结论：

- Credential Manager 选择已有条目；
- 创建新条目并写入 KDBX；
- 数据库锁定时的解锁流程；
- 桌面创建 → 云同步 → Android 使用；
- Android 创建 → 云同步 → 桌面使用；
- debug/release 签名差异；
- Android 13 及以下的 Autofill 回退。

本轮只完成 Android 原生桥的 Kotlin 编译；以上项目不能用编译或 mock 冒充
真机结论，由后续 Android 设备验证记录结果。

## 10. 分阶段交付

1. `omy-password-manager`：公共类型、provider trait、同步密钥生成。
2. KeePassXC transport/crypto/protocol，先完成关联、查询、创建及真实测试。
3. GUI 后端：秘密候选缓存、选择、生成并保存、直接解锁/加密。
4. Android Credential Manager 桥。
5. 中英文用户文档与跨端真实验证。
6. KeePassXC PRF 可用后，再评审 Passkey slot，不与本期混做。
