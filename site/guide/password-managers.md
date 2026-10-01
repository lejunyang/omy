# 密码管理器同步密钥

omy 可以把随机生成的高强度密钥交给第三方密码管理器保存。你不必记忆或复制
这串密钥；密码管理器使用同一份 KDBX 数据库时，它也可以随数据库到达另一台
设备。

这项能力与 Windows Hello 设备密钥互补：

| | 密码管理器同步密钥 | Windows Hello 设备密钥 |
|---|---|---|
| 换设备后可用 | KDBX 已同步并能解锁时可以 | 不可以 |
| 密钥保存位置 | KeePassXC / KeePassDX 等密码库 | 当前机器的 TPM |
| 日常操作 | 从密码管理器选择 | Windows Hello 确认 |
| 丢失后果 | KDBX 与备份都丢失后不可恢复 | 换机、重装或清 TPM 后不可恢复 |

## 桌面 KeePassXC

先在 KeePassXC 的“设置 → 浏览器集成”中启用浏览器集成。omy 使用 KeePassXC
自带的 `keepassxc-proxy` 建立本地加密连接，不需要打开浏览器，也不要求安装
浏览器扩展。

第一次点击“从密码管理器选择”时，KeePassXC 会显示新的关联请求。给连接起一个
能认出的名字（例如 `omy-我的电脑`）并允许访问。以后 omy 只查询 URL 为
`https://credentials.omy.app/` 的条目；第一次读取某个条目时，KeePassXC 仍可能
逐项询问是否允许。

加密时有两种用法：

- “选择已有同步密钥”：从 KeePassXC 已有条目中选择；
- “随机生成并保存”：omy 生成 256 bit 随机密钥，先写入并回读验证，确认可用后
  才开始加密。

KeePassXC 锁住时，omy 可以请求把它带到前台，但不会代替你输入 KDBX 主密码。

## Android 与 KeePassDX

Android 14 及以上使用系统 Credential Manager。点击同一个按钮后，由系统显示
KeePassDX、Google Password Manager 或其它已启用 provider；创建新密钥时也由
系统让你选择保存位置。

Android 13 及以下没有第三方 Credential Provider，继续使用输入框上的 Autofill
或 KeePassDX Magikeyboard。

同一条 KDBX 记录在两端使用不同的自动匹配信息：KeePassXC 按 URL，KeePassDX
绑定 `org.omy.app` 与应用签名。标准 API 不允许普通应用在创建时替另一个平台
写入来源信息，因此“同步 KDBX”会同步秘密值，却不会自动补齐另一端的匹配字段。

- 在 Android 创建后，请在 KeePassDX 的同一条目里补上
  `URL=https://credentials.omy.app/`，再同步到桌面。
- 在桌面创建后，第一次到 Android 请使用普通密码框的 KeePassDX Autofill，
  手动选中同一条目并允许它保存当前应用关联；之后 Android 14+ 的系统选择器
  才能直接找到它。
- 不要在 Android 的“生成并保存”中选择已有桌面条目来完成关联：那会用新生成
  的秘密覆盖旧值，原先用旧秘密加密的文件将无法再靠该条目解锁。

这是 Android 防止应用冒充网站、KeePassXC-Browser 又不能写任意 KDBX 自定义
字段所形成的能力边界，不是云同步失败。正式发布包与调试包的签名不同，也会被
视为两个应用。

## 它仍然是一把密码

首期的同步密钥作为普通 KeePass Password 条目保存，不是 passkey。它由机器随机
生成而非人脑选择，因此强度足够；进入 omy 后继续使用现有 Argon2id 与 key slot
格式，CLI 也可以通过已有密码输入方式使用它。

这项选择让 KeePassXC 与 KeePassDX 在补齐一次匹配信息后共用同一个秘密。
KeePassDX 已支持 WebAuthn PRF，但 KeePassXC 目前只支持 passkey 注册和签名，
不能返回文件加密需要的稳定 PRF 输出。两端都支持后，omy 才会增加真正的
Passkey 解锁。

::: warning KDBX 写入不等于已经同步
“保存成功”只表示条目进入当前打开的密码库。云盘是否上传完成由你的 KDBX 同步
方式决定。第一次使用新密钥后，请确认同步完成，并保留密码或恢复码作为后备。
:::

::: danger 密码库是新的安全边界
谁能解锁你的 KDBX，谁就能取得其中的 omy 同步密钥。请为 KDBX 使用强主密码，
并妥善备份；不要使用仓库测试库的 `123456` 示例密码保护真实数据。
:::
