---
title: LAN sharing
---

# LAN sharing

Share encrypted files from one device to another paired device on the same local network. **Ciphertext only, read only, and keys never leave the machine.**

## What is actually shared

The serving side sends `.omy` ciphertext blocks. The accessing side decrypts them locally using **a password it supplies itself**.

So the channel carries no keys at all. Even if it were fully monitored, an attacker would obtain only ciphertext. The accessing side must already know the password, or the files it retrieves will not open.

## Pairing (once per pair)

Both devices run a command; one waits, the other connects:

```bash
# Device A: wait
omy share pair --listen

# Device B: connect
omy share pair 192.168.1.23:9000
```

Both then display the same PIN. Confirm they match. The protocol is SPAKE2 and the PIN is a single-use random value, discarded after use.

```bash
omy share pair --listen --expires-in 30    # authorization expires in 30 days; 0 = never (default)
omy share pair --listen --pin-file pin.txt # read the PIN from a file instead of typing it
```

::: tip Why the PIN has its own flag
`--pin-file` and `--password-file` are different values: the former is the pairing code, the latter the device store password. Reusing one flag would conflate them.
:::

## Serving a directory

```bash
omy share serve ~/Vault
```

This advertises over mDNS so paired devices on the same subnet discover it automatically.

```bash
omy share serve --name "Study desktop" ~/Vault   # custom advertised name
omy share serve --port 9000 ~/Vault              # fixed port (default 0 = assigned)
omy share serve --no-advertise ~/Vault           # no mDNS; peer enters the address manually
omy share serve --local-only ~/Vault             # loopback only, for self-testing
omy share serve --max-connections 4 ~/Vault      # cap concurrency
```

## Discovering and connecting

```bash
omy share discover
```

Lists devices currently serving on the LAN along with their fingerprints (16 hex characters). Then:

```bash
omy share connect a1b2c3d4e5f60718                    # list remote files
omy share connect a1b2c3d4e5f60718 --fetch ./local    # retrieve to a local directory
omy share connect a1b2c3d4e5f60718 --addr 192.168.1.23:9000   # skip mDNS
```

::: warning --fetch retrieves ciphertext
What arrives are `.omy` files, still encrypted. Viewing the contents still requires the password — which is precisely what "keys never leave the machine" means.
:::

## Managing paired devices

```bash
omy share devices list              # list paired devices
omy share devices revoke <fingerprint>
omy share devices purge             # clear expired authorizations
```

The device store is encrypted and needs a device store password (unrelated to file passwords). `--store` picks the store path; `--password-file` / `--password-env` / `--password-stdin` supply the password.

## How the transport is protected

| Stage | Mechanism |
|---|---|
| Pairing | SPAKE2 — a short PIN yields a shared key, resisting offline dictionary attacks |
| Channel | Noise IK, 1-RTT with forward secrecy |
| Key confirmation | HMAC |
| Static public key exchange | Encrypted during pairing, so a passive observer cannot record that these two devices ever paired |

::: warning About the SPAKE2 implementation
The `spake2` crate states that it is **unaudited by third parties and may not be constant-time**. That is acceptable for pairing: the PIN is single-use, random and discarded, so an attacker cannot accumulate timing information. If your threat model includes local timing side channels, evaluate it yourself.
:::

## In the graphical interface

Device discovery, pairing and browsing remote files are all wired into the GUI. Remote files preview and play directly — playback requests become Range requests against the remote, fetching only the chunks needed.

Remote content is **read only**; the interface offers no encryption, deletion or password changes against remote files.
