---
title: Security boundaries
---

# Security boundaries

::: danger Read this before deciding how to use omy
Every encryption tool has limits. Using one outside its design goals produces a false sense of security.
:::

## Threat model: what is defended

| Scenario | Defence |
|---|---|
| Device lost or stolen | Full AEAD encryption at rest |
| Cloud provider or sync service can read files | Only ciphertext is uploaded; keys never leave the machine |
| Others on a shared device browsing casually | Encrypted filenames plus password unlock |
| Attacker copies ciphertext and brute-forces offline | Argon2id memory-hard KDF |
| Attacker tampers with ciphertext to feed you poisoned content | AEAD authentication plus a header MAC |
| Reordering, truncating or transplanting ciphertext chunks | Nonce bound to chunk index, AAD bound to file UUID, final-chunk marker |
| Header tampering to weaken KDF parameters for brute force | Header MAC covers every parameter |
| Eavesdropping on the same WiFi | End-to-end encrypted channel carrying only ciphertext |

## What is explicitly not defended

::: warning Do not rely on omy for any of the following
:::

**Hiding the fact that this is an encrypted file.** The header carries an obvious magic value (`OMYFILE`). Disguising the extension only stops casual clicking; tools like `binwalk` see through it instantly. omy accepts "the file can be identified as encrypted" as a premise.

**Professional digital forensics.** Memory forensics, cold boot attacks and SSD remnant recovery are all out of scope.

**Nation-state targeted attacks.** These require an entire operational security discipline that software cannot supply.

**A device already carrying malware.** A keylogger simply takes the password. On a rooted system no local encryption means anything.

**Coercion (rubber-hose).** The unprobeability of key slots helps **to a limited degree** — nobody can tell how many passwords a file has — but it cannot withstand sustained physical coercion.

**Fully hiding metadata.** File sizes, counts and modification times are visible by default.

**Screenshots or screen recording while unlocked.** Once content is on screen it is beyond control.

**Secure erasure on SSDs.** Wear levelling and TRIM make overwrite-based erasure physically unreliable, so omy deliberately **does not offer** a "secure delete" feature — promising something unachievable is worse than not offering it.

## Trust boundaries

```
Trusted
├── The user's password (assumed to be kept safely)
├── The operating system kernel running omy
├── The OS key store (Keychain / Keystore / TPM)
└── The omy main process (the only holder of keys)

Semi-trusted (isolated)
└── The FFmpeg subprocess — deprivileged, holds no keys

Untrusted
├── Ciphertext files on disk (assume the attacker has them in full)
├── Cloud sync services
├── The local network (assume it is sniffed)
├── Input files awaiting encryption (may be maliciously crafted)
└── Peer devices on the LAN (given ciphertext, never keys)
```

FFmpeg parses untrusted input (arbitrary media files are a classic attack surface), so it **runs only as a separate subprocess communicating over pipes and never touches any key**. Even if an input file compromises it, there is nothing to steal.

## Command line arguments are a side channel

This is the highest-priority rule for the CLI: **command line arguments are visible to other users on the machine** (`ps aux`, `/proc/*/cmdline`, Task Manager) and enter shell history files.

Hence omy provides no `--password <plaintext>` flag. Available input methods are in [Passwords and key slots](/en/guide/passwords#how-to-supply-a-password).

## Plaintext never lands on disk

Preview and playback go through the custom `omystream://` protocol, decrypting chunks in memory on demand without temporary files.

This property has a precondition, established by measurement: responses must carry `Cache-Control: no-store, no-cache, must-revalidate` and `Pragma: no-cache`, or the WebView may write decrypted data into its disk cache. In the implementation those headers are mandatory, not an optional optimization.

The interface also **does not enable** Tauri's `protocol-asset`, which would expose filesystem paths directly and defeat the purpose.

## Known implementation limits

`omy doctor` reports these honestly rather than hiding them:

**`--cipher xchacha20` is actually ChaCha20-Poly1305** (12-byte nonce), not XChaCha20 (24-byte nonce). Security is unaffected, but it will not interoperate with another implementation's XChaCha20.

**The SPAKE2 implementation is unaudited** and its author states it may not be constant-time. Acceptable for pairing (single-use random PINs, discarded after use), but evaluate it yourself if local timing side channels matter to you.

**Platform verification is limited**: only Windows and Android have been tested; Linux and macOS have not.

## A forgotten password means lost data

There is deliberately no backdoor, master key or recovery phrase.

This is not an oversight: any recovery mechanism is another route around the password, and therefore another attack surface. Back up your passwords yourself.

## High KDF profiles cost both ways

KDF parameters live in the header and must be satisfied at decryption time. A file encrypted with `sensitive` (1 GiB of memory) **will not open** on a device short of memory. That is not a bug; it is inherent to memory-hard KDFs.

## Further reading

The full design derivation lives in [docs/research/](https://github.com/lejunyang/omy/tree/main/docs/research) in the repository; document 01 is the threat model and 07 is the platform and side-channel checklist.
