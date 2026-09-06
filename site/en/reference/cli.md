---
title: Command overview
---

# Command overview

```
omy <command> [options]
```

This page reflects the actual output of `omy --help`. Use `omy <command> --help` at any time for the authoritative version.

## All commands

| Command | Purpose |
|---|---|
| `encrypt` | Encrypt files or directories |
| `decrypt` | Decrypt files |
| `info` | Show file information (partly readable without a password) |
| `verify` | Verify integrity |
| `list` | List `.omy` files in a directory |
| `scan` | Scan a directory, match passwords and list unlockable files |
| `cat` | Decrypt to standard output (pipe friendly) |
| `key` | Key slot management |
| `shard` | Split and merge shards |
| `share` | LAN sharing and access |
| `bench` | KDF and cipher throughput benchmarks |
| `doctor` | Environment self-check |
| `completion` | Generate shell completion scripts |

## Global options

Valid for every subcommand:

| Option | Purpose |
|---|---|
| `-v, --verbose` | Verbose output, repeatable (`-vv`) |
| `-q, --quiet` | Errors only. Conflicts with `-v` |
| `--json` | JSON output for scripts |
| `--no-color` | Disable coloured output |
| `--config <PATH>` | Configuration file path |
| `--yes` | Skip all confirmations (dangerous) |
| `-h, --help` | Help |
| `-V, --version` | Version |

## Password input options

Supported by `encrypt` / `decrypt` / `verify` / `cat` / `info` / `scan` / `key`:

| Option | Notes |
|---|---|
| (none) | Interactive, not echoed. Default and preferred |
| `--password-stdin` | Read from standard input |
| `--password-file <PATH>` | Read the first line of a file |
| `--password-env <VAR>` | Read from an environment variable; pass its **name** |

**There is no `--password <plaintext>`.** Using it yields an explanation and exit code 2. For `scan`, `--password-file` and `--password-env` repeat to supply several passwords.

## encrypt

```
omy encrypt [OPTIONS] <PATHS>...
```

| Option | Notes |
|---|---|
| `-o, --output <PATH>` | Output path (single input only) |
| `--output-dir <DIR>` | Batch output directory |
| `--name-mode <MODE>` | `encrypt` / `keep-ext` / `plain` |
| `--add-password` | Add a key slot, repeatable, up to 8. Interactive only |
| `--kdf-profile <PROFILE>` | `mobile` / `interactive` / `moderate` / `sensitive` |
| `--vault <PATH>` | Join an existing vault, reusing its salt and KDF parameters |
| `--cipher <CIPHER>` | `xchacha20` / `aes256gcm` |
| `--chunk-size <SIZE>` | Such as `256K` / `1M` / `4M` |
| `--compress` | Enable zstd compression |
| `--compress-level <N>` | zstd level 1-19 |
| `--mode <MODE>` | `container` (default) / `tree` |
| `--original <ACTION>` | `keep` (default) / `trash` / `delete` |
| `--thumbnail <MODE>` | `auto` (default) / `none` |
| `--thumbnail-frame <T>` | Video frame timestamp, e.g. `00:01:23` / `83` / `83.5` |
| `--no-media-meta` | Do not write media metadata |
| `--no-moov-cache` | Do not cache the MP4 moov box |

The output name **appends** `.omy` to the full filename: `note.txt` → `note.txt.omy`.

## decrypt

```
omy decrypt [OPTIONS] <FILES>...
```

| Option | Notes |
|---|---|
| `-o, --output <PATH>` | Output path (single input only) |
| `--output-dir <DIR>` | Batch output directory |
| `--stdout` | Write to standard output. Conflicts with `-o` / `--output-dir` |
| `--verify-only` | Check without writing |
| `--ignore-missing-shards` | Emit the available part when shards are missing |

## info

```
omy info [OPTIONS] <FILE>
```

| Option | Notes |
|---|---|
| `--with-password` | Supply a password to see encrypted metadata (original filename etc.) |
| `--extract-thumbnail <PATH>` | Export the thumbnail (password required, WebP format) |

Without a password the public fields are visible: format version, UUID, cipher, KDF parameters, chunk size, chunk count, ciphertext and plaintext sizes. Slot occupancy always reads "unknown" — unprobeable by design.

## verify

```
omy verify [OPTIONS] <FILES>...
```

Checks every chunk's authentication tag and compares the content hash.

## list

```
omy list [OPTIONS] [DIRS]...
```

| Option | Notes |
|---|---|
| `-r, --recursive` | Recurse into subdirectories |
| `--max-depth <N>` | Maximum recursion depth |
| `--any-extension` | Also list files whose header matches but extension is not `.omy` |

Needs no password — it lists files without attempting decryption. Defaults to the current directory.

## scan

```
omy scan [OPTIONS] [DIRS]...
```

| Option | Notes |
|---|---|
| `-r, --recursive` | Recurse (**on by default**) |
| `--no-recursive` | Only the top level |
| `--max-depth <N>` | Maximum recursion depth |
| `--show-locked` | Also list files that did not match |
| `--any-extension` | Also check files not ending in `.omy` (disguised files) |
| `--max-files <N>` | Cap on files examined per scan |

Listed names are the **decrypted originals**.

## cat

```
omy cat [OPTIONS] <FILE>
```

| Option | Notes |
|---|---|
| `--range <RANGE>` | Emit only this range, `start-end`, endpoints inclusive |

Supports the HTTP Range suffix form: `--range -256` takes the last 256 bytes.

## key

```
omy key <COMMAND> <FILE>
```

| Subcommand | Purpose |
|---|---|
| `add` | Add a key slot (requires one existing password) |
| `remove` | Keep only the current password, voiding all others |
| `list` | Show slot occupancy |
| `change` | Change the password (the old one stops working) |
| `reencrypt` | Re-encrypt: replace the file key and rewrite the payload (may change the password too) |

`add` and `change` take the new password via `--new-password-file` / `--new-password-env`.

`key list` **accepts no password flag** — occupancy is unprobeable anyway, so a password adds nothing.

## shard

```
omy shard <COMMAND>
```

| Subcommand | Purpose |
|---|---|
| `split` | Split an encrypted file into shards |
| `merge` | Merge shards |
| `check` | Check shard integrity and gaps |

Options for `split`:

| Option | Notes |
|---|---|
| `--size <SIZE>` | **Required**, size per shard, e.g. `4095M` |
| `--output-dir <DIR>` | Output directory, defaults to alongside the source |
| `--redundant-header` | Include a redundant header per shard (**on by default**) |
| `--no-redundant-header` | Omit redundant headers |
| `--remove-source` | Delete the source after splitting |

## share

```
omy share <COMMAND>
```

| Subcommand | Purpose |
|---|---|
| `serve <DIR>` | Share a directory, awaiting paired devices |
| `discover` | Search the LAN for devices currently sharing |
| `pair [ADDR]` | Pair with another device (both sides run it) |
| `connect <FINGERPRINT>` | Connect to a paired device, list or fetch files |
| `devices list/revoke/purge` | Manage paired devices |

For these subcommands `--password-*` refers to the **device store password**, unrelated to file passwords. The pairing code uses the separate `--pin-file`.

Common `serve` options: `--name`, `--port` (0 = system-assigned), `--local-only`, `--no-advertise`, `--max-connections`, `--store`.

Common `connect` options: `--addr` (skip mDNS), `--fetch <DIR>` (fetch ciphertext without decrypting).

Common `pair` options: `--listen`, `--port`, `--expires-in <DAYS>` (0 = never), `--pin-file`.

## bench

```
omy bench [OPTIONS]
```

| Option | Notes |
|---|---|
| `--profile <PROFILE>` | Test only these profiles, repeatable; all by default |
| `--quick` | Skip the memory-hungry profiles (`moderate` / `sensitive`) |
| `--throughput-size <SIZE>` | Throughput test size, default `64M` |

## doctor

```
omy doctor [--probe-kdf]
```

Reports the platform, AES hardware acceleration, CPU parallelism, configuration file location, interface language, measured KDF timings, FFmpeg availability, LAN sharing availability, and known implementation limits.

Run it once after installing; it is more reliable than documentation.

## completion

```
omy completion <SHELL>
```

Supports `bash` / `elvish` / `fish` / `powershell` / `zsh`.

```bash
omy completion bash > /etc/bash_completion.d/omy
omy completion powershell | Out-String | Invoke-Expression
```
