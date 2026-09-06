---
title: Exit codes
---

# Exit codes

Scripts can distinguish "wrong password" from "corrupt file" — a precision the graphical interface cannot offer.

| Code | Meaning | Typical trigger |
|---|---|---|
| 0 | Success | — |
| 1 | General error | File missing, read or write failure |
| 2 | Usage error | Misspelled option, or using `--password` |
| 3 | Wrong password / no matching slot | `decrypt` or `cat` with a bad password |
| 4 | File corrupt or tampered with | AEAD authentication failure, content hash mismatch |
| 5 | Unsupported format version | Opening a newer format with an older omy |
| 6 | Missing shards | Incomplete shards without `--ignore-missing-shards` |
| 7 | Insufficient permissions | No rights on the target path |
| 8 | Cancelled by the user | Declined an interactive confirmation |

## Measured behaviour

The following comes from actual runs. Note that `verify` and `decrypt` return **different codes** for a wrong password:

| Command | Situation | Exit code |
|---|---|---|
| `omy decrypt --password-file wrong.txt f.omy` | Wrong password | **3** |
| `omy cat --password-file wrong.txt f.omy` | Wrong password | **3** |
| `omy verify --password-file wrong.txt f.omy` | Wrong password | **4** |
| `omy info nosuch.omy` | File missing | 1 |
| `omy encrypt --password x f.txt` | Plaintext password flag | 2 |
| `omy verify --password-file right.txt f.omy` | Normal | 0 |

::: warning verify returns 4, not 3, for a wrong password
`verify` answers the question "is this file intact". It attempts decryption and compares the content hash; with the wrong password it cannot produce the right content, so it reports a verification failure (4), noting in a warning that the real cause was no matching key slot.

So **the exit code of `verify` is not a reliable way to test whether a password is correct**. Use `decrypt --verify-only`, which returns 3 for a wrong password.
:::

## Using them in scripts

```bash
omy decrypt --password-stdin archive.omy < pw.txt
case $? in
  0) echo "ok" ;;
  3) echo "wrong password" ;;
  4) echo "corrupt or tampered" ;;
  6) echo "missing shards" ;;
  *) echo "other error" ;;
esac
```

PowerShell:

```powershell
& omy decrypt --password-file pw.txt archive.omy
switch ($LASTEXITCODE) {
    0 { 'ok' }
    3 { 'wrong password' }
    4 { 'corrupt or tampered' }
    default { "other error ($LASTEXITCODE)" }
}
```

## With --json

`--json` writes structured error information to standard output:

```json
{
  "error": {
    "code": "GENERAL_ERROR",
    "exit_code": 1,
    "message": "读取 note.omy 失败"
  }
}
```

::: tip code is always an English constant
`error.code` does not change with interface language (`WRONG_PASSWORD`, `GENERAL_ERROR` and so on), whereas `message` is localized human-readable text.

**Scripts should match `code` or the exit code, not `message`** — the latter varies with language settings and versions.
:::

The fields of `info --json` are part of a stable contract and feed straight into `jq`:

```bash
omy --json info file.omy | jq -r '.cipher, .chunk_size, .plaintext_size'
# chacha20-poly1305
# 262144
# 9670
```
