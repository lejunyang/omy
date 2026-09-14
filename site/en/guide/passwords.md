---
title: Passwords and key slots
---

# Passwords and key slots

## How to supply a password

In order of preference:

| Method | Flag | Notes |
|---|---|---|
| Interactive (default) | none | Not echoed, never in history |
| Standard input | `--password-stdin` | Suits CI secrets |
| File | `--password-file <path>` | Reads the first line; `chmod 600` recommended |
| Environment variable | `--password-env <name>` | Pass the variable **name**, not the value |

**There is no `--password <plaintext>` flag.** Trying it produces an explanation and exit code 2:

```
不支持 --password 参数

  命令行参数对同机其它用户可见（ps aux / 任务管理器），
  且会被记入 shell 历史文件。
```

The reason: command line arguments are visible to other users on the machine and get recorded in shell history files.

::: warning Environment variables are not fully safe either
`--password-env` passes a name rather than a value, avoiding the visible-argument problem, but the environment itself can be read via `/proc/<pid>/environ` on some systems. Prefer `--password-stdin` in CI.
:::

## Several passwords on one file

Every `.omy` has **8 key slots**. Each independently wraps the same file encryption key (FEK), so any one password opens the file, and none of them needs to know about the others.

Configure several at encryption time:

```bash
omy encrypt --add-password --add-password file.mp4
```

`--add-password` repeats, prompting for one more password each time (interactive only, so multiple passwords cannot get crossed when read from one non-interactive source). Including the main password, the maximum is 8.

Add one later:

```bash
omy key add file.omy
```

You must supply an existing password first (proving you have access), then the new one.

## Inspecting slot usage

```bash
omy key list file.omy
```

```
文件        note.txt.omy
Slot 总数   8
Slot 占用   未知（设计上不可探测）
```

::: tip This is not a missing feature
"You cannot tell how many passwords exist" is deliberate. Empty slots are filled with random bytes, byte-indistinguishable from real ones. Someone holding the file cannot determine how many passwords it has, or whether a second password you would rather not admit to exists.

The cost is that you cannot check either — you have to remember what you configured.
:::

`key list` **takes no password flag**: since slot occupancy is unprobeable by design, a password would reveal nothing extra.

## Changing passwords

```bash
omy key change file.omy      # invalidate the old password, set a new one
omy key remove file.omy      # keep only the current password, void all others
```

`change` only re-wraps the FEK and **does not re-encrypt the payload**, so it completes instantly regardless of file size.

That also means **the file key has not changed**. If you fear an old password already leaked and the holder may have extracted the FEK, changing the password is not enough — replace the FEK:

```bash
omy key reencrypt file.omy
```

This generates a new file key and rewrites the whole payload (optionally changing the password too). On large files it costs about as much as encrypting again.

| Situation | Use |
|---|---|
| Want a password that is easier to remember | `key change` |
| Stop a collaborator from having further access | `key remove` |
| Suspect the password leaked and the file may have been copied | `key reencrypt` |

## Vaults and salts

Files in one vault share a vault salt and KDF parameters. With the same password, Argon2 runs once for the whole vault, which makes scanning a directory fast.

Without `--vault`, each run generates a new salt and each file becomes its own vault — even with identical passwords, scanning must run Argon2 separately per vault (tens to hundreds of milliseconds each, which adds up).

```bash
omy encrypt --vault photos.omy new.jpg
omy encrypt --vault ./vault-dir/ new.jpg
```

## Scanning with multiple passwords

```bash
omy scan --password-file pw1.txt --password-file pw2.txt ~/Documents
```

Both `--password-file` and `--password-env` repeat. omy tries every password against every file and lists what opens, showing decrypted original filenames.

```bash
omy scan --show-locked .        # include files that did not open
omy scan --any-extension .      # also check files not ending in .omy
omy scan --max-files 5000 .     # cap how many files one scan touches
```

`--any-extension` also reads the header of files that do not end in `.omy` — recognition uses the header magic (`OMYFILE` plus a version byte), not the extension, so renamed files are still found.

## Desktop: loading several passwords at once

The password box in the app **adds** a password rather than replacing one. Entering another does not evict the ones already loaded:

1. Enter password A → files encrypted with A appear
2. Enter password B → A's files **stay visible**, and B's files appear alongside them

The status bar shows how many passwords are currently loaded. Entering the same password twice counts once — the check is on the key fingerprint, not on whatever name you gave it — and the app says the password is already in use instead of pretending to add it.

This is what [deniability](./security.md) looks like day to day: a real password and a decoy password can be loaded at the same time, each showing its own files, while the files themselves reveal nothing about how many passwords any vault has.

Locking via the 🔒 in the status bar clears every loaded password at once; there is no half-locked state.

::: tip Not the same as "several passwords on one file"
The earlier section is about how many passwords can open **one file** (8 key slots). This one is about how many passwords **one session** holds and tries. They are independent: you can browse with three passwords while every individual file carries only one.
:::

## Forgetting the password

There is no recovery path. By design there is no backdoor, master key or recovery phrase.

This is not an oversight: any recovery mechanism is another route around the password, which is to say another attack surface. Back your passwords up yourself.
