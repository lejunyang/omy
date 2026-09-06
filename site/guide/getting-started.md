---
title: 快速上手
---

# 快速上手

本页的每条命令与输出都取自实际运行结果，可以照着敲。

## 加密第一个文件

```bash
omy encrypt note.txt
```

会提示输入密码（不回显），然后：

```
✓ 已加密 note.txt → note.txt.omy（9.44 KiB → 10.1 KiB）
```

::: tip 输出文件名的规则
`.omy` 是**整体追加**在原文件名之后的：`note.txt` → `note.txt.omy`，不是替换后缀。这样解密时不必猜原来的扩展名。用 `-o` 可以自己指定。
:::

体积略有增长，来自文件头（本例 656 字节）和每块的认证标签。

## 不用密码也能看到的信息

```bash
omy info note.txt.omy
```

```
文件            note.txt.omy
格式            OMYFILE v1.0
UUID          0dd92161-aa3c-2edd-40b9-b4bab702ae53
文件头长度         656 B
加密算法          ChaCha20-Poly1305
KDF           Argon2id  m=64.0 MiB t=3 p=1
分块大小          256 KiB
块数量           1
压缩            无
密文大小          9.46 KiB (9,686 B)
明文大小          9.44 KiB (9,670 B)
缩略图           无
文件名           已加密
Slot 占用       未知（设计上不可探测）
```

这些是**故意公开**的：解密方需要知道用哪个算法、哪些 KDF 参数才能解开，藏起来没有意义。真正敏感的（原文件名、媒体信息）需要密码，用 `--with-password` 查看。

注意 `Slot 占用` 是「未知」——密钥槽区始终被填满，真实槽与随机填充在字节层面无法区分，所以**看不出这个文件配了几个密码**。

## 验证完整性

```bash
omy verify note.txt.omy
```

```
✓ 校验通过 note.txt.omy（9.44 KiB，1 块）
```

这会逐块验证认证标签，并比对解密后的内容哈希。改动过一个字节的文件通不过。

## 解密

```bash
omy decrypt note.txt.omy
```

默认还原到原文件名。加 `-o` 指定输出路径：

```bash
omy decrypt -o restored.txt note.txt.omy
```

还原结果与原文件 **bit-for-bit 一致**——本例中两者 SHA256 相同。

## 一次加密整个目录

默认是 `container` 模式，把整个目录打包成**单个** `.omy`，目录结构完全隐藏：

```bash
omy encrypt photos/
# ✓ 已加密 photos → photos.omy
```

若希望保持目录结构、逐个文件加密（便于增量同步）：

```bash
omy encrypt --mode tree photos/
```

::: warning 分批加密同一个目录要带 --vault
不指定 `--vault` 时，每次加密都会生成新的随机 salt，产出的文件**各自成一库**。即使密码相同，扫描时也要为每个库单独跑一次 Argon2。分批往同一个文件夹里加东西时，用 `--vault` 指向已有的任一文件：

```bash
omy encrypt --vault photos.omy new-photo.jpg
```
:::

## 一次密码，找出所有能解开的文件

```bash
omy scan .
```

```
✓ note.txt                               9.44 KiB  .\note.txt.omy

检查文件: 1   找到 omy 文件: 1   成功解锁: 1   未匹配: 0   用时: 0.08 s
```

注意左边显示的是**解密后的真实文件名**。`scan` 可以接多个密码（重复 `--password-file` 或 `--password-env`），能解开的自动列出来。加 `--show-locked` 会把没解开的也列出来。

`list` 与它不同——只列文件不试密码，因此不需要密码：

```bash
omy list
# .\note.txt.omy      10.1 KiB      1 块
```

## 脚本里怎么用

密码有四种非交互输入方式。**不存在 `--password` 明文参数**，因为命令行参数对同机其他用户可见（`ps aux` / 任务管理器）且会进 shell history：

```bash
# CI：从管道读
echo "$SECRET" | omy encrypt --password-stdin file.mp4

# 本地脚本：从文件读（建议 chmod 600）
omy encrypt --password-file ~/.omy-pw file.mp4

# 从环境变量读——注意传的是变量名，不是值
OMY_PW=secret omy encrypt --password-env OMY_PW file.mp4
```

配合 `--json` 与退出码，脚本能精确区分失败原因：

```bash
omy --json verify archive.omy
if [ $? -eq 3 ]; then echo "密码不对"; fi
```

完整的退出码含义见[退出码](/reference/exit-codes)。

## 下一步

- [加密与解密](/guide/encrypt-decrypt)：算法、分块大小、压缩、原文件处置
- [密码与密钥槽](/guide/passwords)：一个文件挂多个密码、改密码、重新加密
- [配置文件](/guide/configuration)：把常用参数写成默认值
