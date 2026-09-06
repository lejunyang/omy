---
title: 局域网共享
---

# 局域网共享

把一台设备上的加密文件共享给同一局域网内另一台已配对的设备。**只传密文、只读，密钥不出本机。**

## 它到底共享了什么

共享方发出去的是 `.omy` 密文块。访问方拿到密文之后，用**自己输入的密码**在本地解密。

这意味着：共享通道本身不传递任何密钥，即使通道被完全监听，攻击者也只拿到密文。访问方必须自己知道密码，否则取回来的文件打不开。

## 配对（只需一次）

两台设备都要执行，一方等待一方连入：

```bash
# 设备 A：等待
omy share pair --listen

# 设备 B：连过去
omy share pair 192.168.1.23:9000
```

双方会看到同一个配对码（PIN），核对一致后确认。协议用 SPAKE2，PIN 是一次性随机值、用完即弃。

```bash
omy share pair --listen --expires-in 30    # 授权 30 天后过期，0 为永久（默认）
omy share pair --listen --pin-file pin.txt # 从文件读 PIN，避免交互
```

::: tip 为什么 PIN 要单独一个参数
`--pin-file` 与 `--password-file` 是两个不同的值：前者是配对码，后者是设备库密码。复用同一个参数会让两者混在一起。
:::

## 共享目录

```bash
omy share serve ~/Vault
```

会通过 mDNS 广播，同网段的已配对设备能自动发现。

```bash
omy share serve --name "书房台式机" ~/Vault   # 自定义广播名
omy share serve --port 9000 ~/Vault          # 固定端口（默认 0 = 系统分配）
omy share serve --no-advertise ~/Vault       # 不广播，对方需手动输地址
omy share serve --local-only ~/Vault         # 只监听回环，用于自测
omy share serve --max-connections 4 ~/Vault  # 限制并发
```

## 发现与连接

```bash
omy share discover
```

列出局域网内正在共享的设备及其指纹（16 位十六进制）。然后：

```bash
omy share connect a1b2c3d4e5f60718                    # 列出远端文件
omy share connect a1b2c3d4e5f60718 --fetch ./local    # 取回到本地
omy share connect a1b2c3d4e5f60718 --addr 192.168.1.23:9000   # 跳过 mDNS
```

::: warning --fetch 取回的仍是密文
取回来的是 `.omy` 文件，没有解密。要看内容还得用密码解，这正是「密钥不出本机」的体现。
:::

## 管理已配对设备

```bash
omy share devices list              # 列出已配对设备
omy share devices revoke <指纹>     # 吊销某台设备
omy share devices purge             # 清理已过期的授权
```

设备库是加密存储的，需要设备库密码（与文件密码无关）。用 `--store` 可指定库路径，`--password-file` / `--password-env` / `--password-stdin` 提供库密码。

## 传输是怎么保护的

| 环节 | 机制 |
|---|---|
| 配对 | SPAKE2，短 PIN 换出共享密钥，抗离线字典攻击 |
| 信道 | Noise IK，1-RTT 且提供前向保密 |
| 密钥确认 | HMAC |
| 静态公钥交换 | 配对时加密传输，避免被动观察者记录"这两台设备配过对" |

::: warning 关于 SPAKE2 实现
用的 `spake2` crate 自述**未经第三方审计，且可能不是常量时间实现**。配对场景下可以接受：PIN 是一次性随机值、用完即弃，攻击者无从积累时序信息。但如果你的威胁模型包含本地时序侧信道攻击，请自行评估。
:::

## 图形界面里

设备发现、配对、浏览远端文件都已接入图形界面。远端文件在界面里可以直接预览和播放——播放请求会转成对远端的 Range 请求，只取需要的块。

远端内容是**只读**的，界面上不会提供针对远端文件的加密、删除、改密码等操作。
