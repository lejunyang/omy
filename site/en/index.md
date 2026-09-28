---
layout: home

hero:
  name: omy
  text: Encrypted, and still directly usable
  tagline: Seek freely through encrypted video, preview images, audio and text inline, and see real decrypted filenames and thumbnails in the listing. Cross-platform, with the CLI and GUI sharing one core.
  actions:
    - theme: brand
      text: Getting started
      link: /en/guide/getting-started
    - theme: alt
      text: What is omy
      link: /en/guide/what-is-omy
    - theme: alt
      text: GitHub
      link: https://github.com/lejunyang/omy

features:
  - title: Playable while encrypted
    details: Chunked AEAD plus HTTP Range means video seeks anywhere without decrypting the whole file to disk first. Plaintext never lands on disk.
  - title: Filenames are encrypted too
    details: Optionally keep the extension in plaintext so you can still filter by type. The listing shows the original name while the disk holds ciphertext.
  - title: Scan with several passwords
    details: Enter multiple passwords once and every file in the directory that opens will surface. A single file holds up to 8 independent key slots.
  - title: Thumbnails live in the header
    details: The listing only reads the file header to show a thumbnail, so it never decrypts the payload.
  - title: Sharding
    details: Pick the size per shard; each one carries a redundant header by default. Losing the first shard still leaves the metadata recoverable, and missing shards degrade to partial playback.
  - title: LAN sharing
    details: Ciphertext only, read only. SPAKE2 pairing over a Noise IK channel, and keys never leave the machine.
  - title: Bit-for-bit restore
    details: Decryption reproduces the original file exactly, not merely something that looks the same.
  - title: The CLI is first class
    details: Thirteen subcommands sharing the same omy-core as the GUI, with JSON output and precise exit codes.
---

## Three commands to see what it does

```bash
# Encrypt a video. The password is typed interactively, never echoed,
# and never enters shell history.
omy encrypt holiday.mp4        # produces holiday.mp4.omy

# Public information is readable without any password
# (cipher, chunk size, KDF parameters).
omy info holiday.mp4.omy

# Decrypt. The result is bit-for-bit identical to the original.
omy decrypt holiday.mp4.omy
```

## Project status

| crate | License | Purpose |
|---|---|---|
| `omy-core` | MIT OR Apache-2.0 | Format I/O, key hierarchy, chunked AEAD. No FFmpeg dependency |
| `omy-media` | LGPL-2.1+ | Media probing, tiering, thumbnails. Calls FFmpeg as a subprocess |
| `omy-net` | MIT OR Apache-2.0 | Discovery, SPAKE2 pairing, Noise IK channel, ciphertext block service |
| `omy-cli` | GPL-3.0+ | Command line tool, 13 subcommands |
| `omy-gui` | GPL-3.0+ | Tauri v2 + Vue 3 graphical interface |

::: warning Tested platforms
Only **Windows** and **Android** have been tested in practice. The Linux and macOS code paths are implemented — CI builds them continuously and treats them as release gates (a failed build blocks the release) — but they have **never been run on real machines**. Verify for yourself before relying on those two platforms.
:::

::: danger A forgotten password cannot be recovered
There is deliberately no backdoor and no recovery mechanism. Forgetting the password means the data is gone.
:::
