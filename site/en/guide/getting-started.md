---
title: Getting started
---

# Getting started

Every command and output on this page comes from a real run, so you can follow along literally.

## Encrypt your first file

```bash
omy encrypt note.txt
```

You are prompted for a password (not echoed), then:

```
✓ 已加密 note.txt → note.txt.omy（9.44 KiB → 10.1 KiB）
```

::: tip How the output name is formed
`.omy` is **appended to the whole filename**: `note.txt` becomes `note.txt.omy`, rather than replacing the extension. That way decryption never has to guess the original extension. Use `-o` to choose the name yourself.
:::

The slight growth comes from the header (656 bytes here) and the per-chunk authentication tags.

## What is readable without a password

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

These fields are public **on purpose**: whoever decrypts needs to know the algorithm and KDF parameters, so hiding them buys nothing. The genuinely sensitive parts (original filename, media details) require a password via `--with-password`.

Note that `Slot 占用` reads "unknown" — the key slot area is always full, and real slots are byte-indistinguishable from random padding, so **you cannot tell how many passwords a file has**.

::: tip Output language
The messages above are in Chinese because that is this machine's locale. Set `language = "en"` under `[ui]` in the config file, or change your system locale, to get English output. See [Configuration file](/en/guide/configuration).
:::

## Verify integrity

```bash
omy verify note.txt.omy
```

```
✓ 校验通过 note.txt.omy（9.44 KiB，1 块）
```

This checks every chunk's authentication tag and compares the decrypted content hash. A file with a single altered byte fails.

## Decrypt

```bash
omy decrypt note.txt.omy
```

Restores to the original filename by default. Use `-o` for somewhere else:

```bash
omy decrypt -o restored.txt note.txt.omy
```

The result is **bit-for-bit identical** to the original — in this run both files had the same SHA256.

## Encrypt a whole directory

The default `container` mode packs the directory into a **single** `.omy`, hiding the structure entirely:

```bash
omy encrypt photos/
# ✓ 已加密 photos → photos.omy
```

To keep the directory structure and encrypt files individually (which allows incremental sync):

```bash
omy encrypt --mode tree photos/
```

::: warning Adding to the same directory later needs --vault
Without `--vault`, every run generates a fresh random salt and the resulting files each form **their own vault**. Even with the same password, scanning then has to run Argon2 once per vault. When adding to an existing collection, point `--vault` at any existing file:

```bash
omy encrypt --vault photos.omy new-photo.jpg
```
:::

## One password, find everything it opens

```bash
omy scan .
```

```
✓ note.txt                               9.44 KiB  .\note.txt.omy

检查文件: 1   找到 omy 文件: 1   成功解锁: 1   未匹配: 0   用时: 0.08 s
```

The name on the left is the **decrypted original filename**. `scan` accepts several passwords (repeat `--password-file` or `--password-env`) and lists whatever opens. Add `--show-locked` to see the rest too.

`list` is different — it only lists files without trying passwords, so it needs none:

```bash
omy list
# .\note.txt.omy      10.1 KiB      1 块
```

## Using it from scripts

There are four non-interactive ways to supply a password. **There is no `--password` plaintext flag**, because command line arguments are visible to other users on the machine (`ps aux`, Task Manager) and land in shell history:

```bash
# CI: read from a pipe
echo "$SECRET" | omy encrypt --password-stdin file.mp4

# Local script: read from a file (chmod 600 recommended)
omy encrypt --password-file ~/.omy-pw file.mp4

# From an environment variable — pass the variable NAME, not the value
OMY_PW=secret omy encrypt --password-env OMY_PW file.mp4
```

Combined with `--json` and the exit codes, scripts can tell failures apart precisely:

```bash
omy --json verify archive.omy
if [ $? -eq 3 ]; then echo "wrong password"; fi
```

See [Exit codes](/en/reference/exit-codes) for the full list — and note the caveat about `verify` there.

## Next

- [Encrypt and decrypt](/en/guide/encrypt-decrypt)
- [Passwords and key slots](/en/guide/passwords)
- [Configuration file](/en/guide/configuration)
