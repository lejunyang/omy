---
title: What is omy
---

# What is omy

omy is a cross-platform encrypted file manager. It exists to resolve a tension common to encryption tools:

**Once a file is encrypted, it stops being usable.** To glance at one photo inside an encrypted archive you have to extract the whole thing. To scrub through an encrypted movie you either wait for it to decrypt to disk or give up. And the plaintext you extracted now sits on disk, undoing the encryption you just performed.

omy builds "still usable" into the format itself.

## How it differs from the usual approaches

| | Encrypted archive | Encrypted container / virtual disk | omy |
|---|---|---|---|
| View one image | Extract the whole archive | Normal read once mounted | Reads only the header for a thumbnail |
| Seek through video | Extract in full first | Works, but needs mount privileges | Seeks directly, nothing lands on disk |
| Plaintext on disk | Yes | Visible system-wide while mounted | Never written |
| Needs admin rights | No | Usually | No |
| Single file usable alone | Bound to the archive | Bound to the container | Every `.omy` is self-contained |

The trouble with containers is granularity: once mounted, the entire volume is plaintext to every process on the system. Each `.omy` file stands on its own — move one elsewhere and the same password still opens it.

## Core mechanisms

**Chunked AEAD.** The payload is split into fixed-size chunks (256 KiB by default), each encrypted independently with its own authentication tag. Reading the frame at second 100 decrypts only the few chunks involved, which is what makes arbitrary seeking possible. The header carries enough information to compute chunk offsets in O(1).

**Custom protocol plus HTTP Range.** The player in the GUI fetches data over an `omystream://` protocol, and the handler decrypts and returns only the requested byte range. This path has been measured: six out-of-order seeks all landed accurately, and the WebView did not write decrypted data into its disk cache.

**Two-stage key derivation.** The password goes through Argon2id to produce a KEK, then HKDF derives a per-slot wrapping key. The file encryption key (FEK) that actually encrypts the payload is random and stored wrapped in a key slot. Changing the password only re-wraps the FEK — the payload is untouched.

**Thumbnails inside the header.** Encryption generates a thumbnail (scaling the image, or grabbing a video frame) and stores it in the header's TLV area. The listing shows it by reading the header alone, never touching the payload.

## Two front ends, one core

Both the command line (`omy`) and the graphical interface (`omy-gui`) sit on top of `omy-core` and behave identically. The CLI targets scripts, servers and batch work, with JSON output and precise exit codes; the GUI provides browsing, inline preview and playback.

They operate on the same files and can hand off to each other: a file encrypted by the CLI browses and plays in the GUI, and vice versa.

## Good fits and bad fits

**Good fits**: photos, videos and documents you would rather not have casually browsed; encrypted copies on an external drive or cloud storage; a media library you still want to watch while it stays encrypted.

**Bad fits**: anything needing deniable encryption (hidden volumes); anything needing resistance to memory forensics or malware; treating `.omy` as your only backup — a forgotten password has no recovery path whatsoever.

For the boundaries of the threat model, see [Security boundaries](/en/guide/security).

## Next

- [Installation](/en/guide/install)
- [Getting started](/en/guide/getting-started)
- [Command overview](/en/reference/cli)
