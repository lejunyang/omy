---
title: 退出码
---

# 退出码

脚本可以据此区分「密码错」和「文件坏」——这是图形界面无法提供的精度。

| 码 | 含义 | 典型触发 |
|---|---|---|
| 0 | 成功 | — |
| 1 | 一般错误 | 文件不存在、读写失败 |
| 2 | 参数错误 | 拼错选项、用了 `--password` |
| 3 | 密码错误 / 无匹配 slot | `decrypt` / `cat` 密码不对 |
| 4 | 文件损坏或被篡改 | AEAD 认证失败、内容哈希不匹配 |
| 5 | 格式版本不支持 | 用旧版 omy 打开新版格式 |
| 6 | 缺少分片 | 分片不全且未加 `--ignore-missing-shards` |
| 7 | 权限不足 | 无权读写目标路径 |
| 8 | 用户取消 | 交互确认时选了否 |

## 实测行为

以下是实际运行的结果，注意 `verify` 与 `decrypt` 在密码错误时返回**不同的码**：

| 命令 | 情况 | 退出码 |
|---|---|---|
| `omy decrypt --password-file wrong.txt f.omy` | 密码错 | **3** |
| `omy cat --password-file wrong.txt f.omy` | 密码错 | **3** |
| `omy verify --password-file wrong.txt f.omy` | 密码错 | **4** |
| `omy info nosuch.omy` | 文件不存在 | 1 |
| `omy encrypt --password x f.txt` | 用了明文密码参数 | 2 |
| `omy verify --password-file right.txt f.omy` | 正常 | 0 |

::: warning verify 密码错返回 4 而不是 3
`verify` 的语义是「这个文件完好吗」。它试图解密并比对内容哈希，密码不对时解不出正确内容，报告的是「校验失败」（4），并在警告里说明真实原因是没有匹配的密钥槽。

所以**用 `verify` 的退出码判断密码对不对是不可靠的**。要区分这两种情况，用 `decrypt --verify-only`，它在密码错时返回 3。
:::

## 在脚本里用

```bash
omy decrypt --password-stdin archive.omy < pw.txt
case $? in
  0) echo "成功" ;;
  3) echo "密码不对" ;;
  4) echo "文件损坏或被篡改" ;;
  6) echo "分片不全" ;;
  *) echo "其他错误" ;;
esac
```

PowerShell：

```powershell
& omy decrypt --password-file pw.txt archive.omy
switch ($LASTEXITCODE) {
    0 { '成功' }
    3 { '密码不对' }
    4 { '文件损坏或被篡改' }
    default { "其他错误（$LASTEXITCODE）" }
}
```

## 配合 --json

`--json` 会把结构化的错误信息打到标准输出：

```json
{
  "error": {
    "code": "GENERAL_ERROR",
    "exit_code": 1,
    "message": "读取 note.omy 失败"
  }
}
```

::: tip code 恒为英文常量
`error.code` 不随界面语言变化（`WRONG_PASSWORD`、`GENERAL_ERROR` 等），而 `message` 是本地化的人类可读文本。

**脚本应该匹配 `code` 或退出码，不要匹配 `message`**——后者会随语言设置和版本变化。
:::

`info --json` 的字段是稳定契约的一部分，可以直接喂给 `jq`：

```bash
omy --json info file.omy | jq -r '.cipher, .chunk_size, .plaintext_size'
# chacha20-poly1305
# 262144
# 9670
```
