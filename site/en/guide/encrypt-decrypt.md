---
title: Encrypt and decrypt
---

# Encrypt and decrypt

## Choosing a cipher

```bash
omy encrypt --cipher aes256gcm file.mp4
```

| Value | When to use it |
|---|---|
| `xchacha20` (default) | Fast even without AES hardware acceleration; a safe default |
| `aes256gcm` | Faster when the CPU has AES-NI |

If unsure, run `omy doctor` — it states outright whether this machine has AES-NI, along with measured KDF timings.

::: warning About the name xchacha20
`--cipher xchacha20` actually uses **ChaCha20-Poly1305** (12-byte nonce), not XChaCha20 (24-byte nonce). `omy doctor` reports this explicitly. Neither is a security problem, but if you need to interoperate with another implementation's XChaCha20, mind the difference.
:::

## KDF profiles

Passwords go through Argon2id, deliberately made slow to resist brute force. The profile decides how slow:

| Profile | Memory | Iterations | For |
|---|---|---|---|
| `mobile` | 32 MiB | 4 | Mobile devices |
| `interactive` | 64 MiB | 3 | **Default**, the OWASP baseline |
| `moderate` | 256 MiB | 4 | Stronger, but decryption needs the same memory |
| `sensitive` | 1 GiB | 4 | Strongest |

```bash
omy encrypt --kdf-profile moderate secret.pdf
```

::: danger High profiles cost you in both directions
The parameters are written into the header and must be met at decryption time. A file encrypted with `sensitive` **will not open** on a device with only 512 MiB available. Pick `mobile` or `interactive` for anything you intend to view on a phone.
:::

`omy bench` measures each profile on this machine; `--quick` skips the memory-hungry ones.

## Chunk size

```bash
omy encrypt --chunk-size 1M movie.mkv
```

The default is 256 KiB. Chunks are the granularity of random access: smaller chunks waste less when seeking, but mean more authentication tags and slightly more overhead. `1M` or `4M` usually suits large video better.

## Compression

```bash
omy encrypt --compress --compress-level 12 logs.txt
```

Uses zstd, levels 1–19 (default 3), compressing before encrypting.

**Do not enable it for already-compressed content** (mp4, jpg, zip) — it saves nothing and burns CPU. Text, logs and uncompressed backups benefit clearly.

## Directories: container or tree

```bash
omy encrypt --mode container photos/   # default
omy encrypt --mode tree photos/
```

| | `container` | `tree` |
|---|---|---|
| Output | One `.omy` | One `.omy` per file, structure preserved |
| Directory structure | Fully hidden | Directory names visible (filenames still encrypted) |
| Leaks file count | No | Yes |
| Incremental sync | Changing one file repacks everything | Only changed files are re-encrypted |
| Extract one file | Unpack the whole thing | Decrypt just that one |

Pick `container` for long-term archives on cloud storage; pick `tree` when files change often or a sync tool is involved.

## How filenames are handled

```bash
omy encrypt --name-mode keep-ext video.mp4
```

| Value | Result |
|---|---|
| `encrypt` (default) | Filename and extension both encrypted |
| `keep-ext` | Filename encrypted, extension left in plaintext for filtering and sorting |
| `plain` | Filename not encrypted |

`keep-ext` trades a little privacy for convenience: someone can see this is an mp4, but not what it contains.

Filenames are padded to a multiple of 64 bytes before encryption, so ciphertext length reveals nothing about the original name's length either.

## What happens to the original

```bash
omy encrypt --original trash photos/     # move to trash
omy encrypt --original delete photos/    # delete permanently
```

The default is `keep`. The other two require confirmation, or `--yes` to skip it.

::: tip The ordering is safe
Both `trash` and `delete` **verify that the encrypted output decrypts correctly before touching the original**. Done the other way round, a bad encryption would leave you with nothing.
:::

Android has no usable trash (the underlying crate provides no implementation there), so only `keep` and `delete` apply.

## Decrypting

```bash
omy decrypt file.omy                    # restore to the original name
omy decrypt -o out.mp4 file.omy         # explicit output
omy decrypt --output-dir ./out *.omy    # batch
omy decrypt --stdout file.omy | vlc -   # pipe to a player, nothing on disk
omy decrypt --verify-only file.omy      # check only, write nothing
```

For a slice of the content, `cat` takes byte ranges:

```bash
omy cat --range 0-1023 file.omy      # first 1024 bytes
omy cat --range -256 file.omy        # last 256 bytes
```

Range endpoints are inclusive, following the HTTP Range convention.

## Degraded decryption with missing shards

```bash
omy decrypt --ignore-missing-shards partial.omy
```

When shards are lost, this emits whatever can still be decrypted. For video that usually still plays most of the way through. See [Sharding](/en/guide/sharding).
