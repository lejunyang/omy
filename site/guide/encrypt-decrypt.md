---
title: 加密与解密
---

# 加密与解密

## 选择算法

```bash
omy encrypt --cipher aes256gcm file.mp4
```

| 取值 | 何时用 |
|---|---|
| `xchacha20`（默认） | 没有 AES 硬件加速时依然快，是安全的默认选择 |
| `aes256gcm` | CPU 有 AES-NI 时更快 |

不确定选哪个就跑 `omy doctor`，它会直接告诉你本机有没有 AES-NI，以及各档位的实测耗时。

::: warning 关于 xchacha20 这个名字
`--cipher xchacha20` 实际使用的是 **ChaCha20-Poly1305**（12 字节 nonce），而不是 XChaCha20（24 字节 nonce）。`omy doctor` 会明确报告这一点。两者安全性都没问题，但如果你需要与其他实现的 XChaCha20 互操作，注意这个差异。
:::

## KDF 档位

密码要经 Argon2id 派生，这一步刻意做得慢，以抵抗暴力破解。档位决定有多慢：

| 档位 | 内存 | 迭代 | 适用 |
|---|---|---|---|
| `mobile` | 32 MiB | 4 | 移动设备 |
| `interactive` | 64 MiB | 3 | **默认**，OWASP 基线 |
| `moderate` | 256 MiB | 4 | 更强，但解密时也要吃同样的内存 |
| `sensitive` | 1 GiB | 4 | 最强 |

```bash
omy encrypt --kdf-profile moderate secret.pdf
```

::: danger 高档位是双向成本
参数写进文件头，解密时必须用同样的参数。用 `sensitive` 加密的文件，在只有 512 MiB 可用内存的设备上**打不开**。给手机上要看的东西选 `mobile` 或 `interactive`。
:::

`omy bench` 可以实测各档位在本机的耗时；`--quick` 跳过高内存档位。

## 分块大小

```bash
omy encrypt --chunk-size 1M movie.mkv
```

默认 256 KiB。分块是随机访问的粒度：块越小，seek 时多解密的浪费越少，但认证标签的数量越多、体积开销略大。播放大视频时 `1M` 或 `4M` 通常更合适。

## 压缩

```bash
omy encrypt --compress --compress-level 12 logs.txt
```

用 zstd，级别 1–19（默认 3），先压再加密。

对已经压缩过的内容（mp4、jpg、zip）**不要开**——省不下空间，只是白费 CPU。文本、日志、未压缩的备份才有明显收益。

## 目录：container 还是 tree

```bash
omy encrypt --mode container photos/   # 默认
omy encrypt --mode tree photos/
```

| | `container` | `tree` |
|---|---|---|
| 产物 | 单个 `.omy` | 每个文件一个 `.omy`，保持目录结构 |
| 目录结构 | 完全隐藏 | 目录名可见（文件名仍加密） |
| 文件数量泄露 | 不泄露 | 泄露 |
| 增量同步 | 改一个文件要重打整包 | 只重新加密改动的文件 |
| 单独取一个文件 | 要先解整包 | 直接解那一个 |

放到网盘上做长期归档选 `container`；需要经常增删、和同步工具配合的选 `tree`。

## 文件名怎么处理

```bash
omy encrypt --name-mode keep-ext video.mp4
```

| 取值 | 结果 |
|---|---|
| `encrypt`（默认） | 文件名连后缀一起加密 |
| `keep-ext` | 文件名加密，后缀留明文，便于按类型筛选排序 |
| `plain` | 文件名不加密 |

`keep-ext` 是便利与隐私的折中：能看出这是个 mp4，但看不出是什么内容。

文件名在加密前会填充到 64 字节的整数倍，所以从密文长度也推不出原文件名有多长。

## 加密后怎么处置原文件

```bash
omy encrypt --original trash photos/     # 移到回收站
omy encrypt --original delete photos/    # 永久删除
```

默认 `keep`（保留）。另两个都需要确认，或者加 `--yes` 跳过。

::: tip 顺序上是安全的
`trash` 与 `delete` 都会**先验证加密产物能正常解密，再动原文件**。反过来一旦加密有问题，原件已经没了。
:::

Android 上没有回收站可用（依赖的 crate 在该平台不提供实现），所以那里只有 `keep` 与 `delete`。

## 解密

```bash
omy decrypt file.omy                    # 还原到原文件名
omy decrypt -o out.mp4 file.omy         # 指定输出
omy decrypt --output-dir ./out *.omy    # 批量
omy decrypt --stdout file.omy | vlc -   # 管道给播放器，不落盘
omy decrypt --verify-only file.omy      # 只校验，不写出
```

只要一部分内容时用 `cat`，它支持按字节范围取：

```bash
omy cat --range 0-1023 file.omy      # 前 1024 字节
omy cat --range -256 file.omy        # 最后 256 字节
```

范围端点按 HTTP Range 惯例含两端。

## 缺片时的降级解密

```bash
omy decrypt --ignore-missing-shards partial.omy
```

分片丢了几片时，把能解的部分输出出来。对视频来说通常仍可播放大部分内容。详见[分片](/guide/sharding)。
