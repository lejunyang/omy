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

标准安装目录会自动探测。便携版请到“设置 → 安全与密码 → KeePassXC 便携版”
填写 `keepassxc-proxy.exe` 的完整路径（例如
`E:\KeePassXC\keepassxc-proxy.exe`）。显式路径失效时，omy 会直接报错，
不会悄悄启动 PATH 或标准目录中的另一份同名程序。

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

Android 目前通过普通密码框使用 KeePassDX Autofill 或 Magikeyboard，不显示“从密码
管理器选择”和“生成并保存”入口。将 KeePassXC 中的同步密钥条目同步到手机后，
在密码框使用 KeePassDX 选择同一条目即可；密钥仍会由现有 Argon2id 与 key slot
流程处理。

暂不启用 Android 14+ Credential Manager 的显式密码 API，是因为实测 KeePassDX
4.5.5 创建的条目只写入 `AndroidApp=org.omy.app`，没有写入配对的受保护字段
`AndroidApp Signature`；而 KeePassDX 在返回密码前又要求包名和签名同时匹配，
因此这些条目虽然会出现在系统选择器里，选择后却固定报 `Password origin do not
match`。系统还会优先显示这个无效候选，无法由调用应用强制打开完整的 KeePassDX
条目选择器。

omy 不会伪造 Web origin、绕过 APK 签名校验，或自行改写第三方 KDBX 的受保护
字段。等 provider 能可靠写入并回读签名字段后，才会重新开放显式选择。正式发布包
与调试包的签名不同，也会被密码管理器视为两个应用。

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
