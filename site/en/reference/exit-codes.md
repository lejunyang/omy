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

## Exit codes for remote and virtual commands

`remote` subcommands reuse these codes, with their own breakdown:

| Situation | Exit code | Notes |
|---|---|---|
| Usage error on `remote` / `remote telegram` / `remote virtual` | 2 | from clap |
| General remote failure (unreachable, read/write error, `telegram check` down) | 1 | lets scripts tell it apart from "bad invocation" (2) |
| Wrong `remote virtual` password | **3** | `VIRTUAL_WRONG_PASSWORD` |
| An interactive confirm on `remote virtual place remove` answered "no" | 8 | same as file commands |
| Other `remote virtual` failure | 1 | see the `VIRTUAL_*` structured codes below |

`remote virtual` errors are structured: `--json` puts a code in `error.code` such as `VIRTUAL_LOCKED` (encrypted place, no password supplied), `VIRTUAL_WRONG_PASSWORD` (wrong password = 3), `VIRTUAL_NOT_ENCRYPTED`, `VIRTUAL_NO_SUCH_PLACE` — match that, not the localized message.

Telegram place operations (`encrypt` / `unlock` / `lock` / `decrypt` / `targets` / `forward` / `search`) always exit 1 on error and are distinguished by a stable bracket prefix in the message: `[tg_unlock_wrong]` wrong password, `[tg_no_protector]` no credential store, refusing plaintext, `[tg_not_encrypted]` place not encrypted, `[tg_no_session]` no session yet, `[tg_cross_chat]` forward entries from different chats. Match the prefix, not the exit code.

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
