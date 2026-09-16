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

### Want to see the slots? Choose manageable mode when encrypting

What is described above is **deniable mode** (the default). At encryption time you can
choose **manageable mode** instead:

```bash
omy encrypt file.txt --slot-mode managed
```

The file then carries an encrypted slot directory recording what each slot holds. Give
`key list` a password and it lists them:

```bash
omy key list file.omy --password-env PW
```

The trade-offs:

| | Deniable (default) | Manageable |
|---|---|---|
| Can others tell how many passwords? | No | No |
| Can **you** tell? | No | Yes, with the password |
| Remove a single password | No, only "keep the current one" | Yes, precisely |
| Does changing a password affect the recovery code? | No | No |
| Can adding a password evict another? | Possibly (no way to detect free slots) | No |

The key point: **to anyone who cannot open the file, both modes protect exactly the
same amount**. The slot directory is encrypted, so reading it requires opening the file
first. What you give up is only the ability to hide, from someone who already has a
password, how many passwords exist.

The mode is fixed at encryption time and **cannot be changed afterwards** — it lives in
the file header, so switching means re-encrypting.

Avoid mixing both modes within one vault: the same command would behave differently on
different files.

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

## Encrypted folders: multiple passwords and `.omy-keys`

A folder encrypted with `--mode tree` supports multiple passwords and a
recovery code just like a single file does, and every one of them opens
the whole tree:

```bash
omy key add folder.omy          # add a password
omy key recovery folder.omy     # one recovery code for the whole tree
```

The tree shares one recovery code rather than one per file — otherwise you
would be copying down N slips of paper, and losing one would cost you a
file, which defeats the point.

### Do not delete `.omy-keys`

Every encrypted folder contains a `.omy-keys` file — the key that unlocks
the **folder name**. Delete it and that name can never be recovered: the
files inside are still there, but all you will see is a string of gibberish.

Keep it with the folder when copying or moving. It lives *inside* the folder
for exactly this reason: it travels with it, so even a single subdirectory
copied out on its own still opens.

It is a fixed 408 bytes regardless of how many passwords you have, so it
does not reveal that count to anyone.

### Changing the password does not rename the folder

The folder name is encrypted with its own directory key, independent of any
password. Change the password and the name stays put; the path you are
holding remains valid.


## Recovery codes: the only way back in

By design there is **no backdoor and no master key** — we cannot open your files
for you. What you can do is attach a recovery code ahead of time:

```bash
omy key recovery secret.omy
```

It prints 26 words. If you forget the password, use them to set a new one:

```bash
omy key restore secret.omy
```

On the desktop, right-click a file: "Generate recovery code…" and "Open with
recovery code…". Generating shows the 26 words once and requires you to tick
"I have written it down" before the dialog will close — we keep no plaintext
copy, so once it closes the words cannot be recovered.

The two entries have different preconditions: **generating** needs the file
unlocked (you have to prove you can open it today), while **using** one does
not — you are reaching for it precisely because the password is gone.

### How a recovery code differs from a password

They differ only in how they become a key; after that they take exactly the
same path:

| | Password | Recovery code |
|---|---|---|
| Source | What you remember | 256 bits of machine-generated entropy |
| Derivation | Argon2id (deliberately slow — entropy is low) | HKDF (microseconds — entropy is already enough) |
| Everyday use | Yes | No; keep it in a drawer |
| Used during folder scans | ✅ | ❌ **No** |
| If lost | The recovery code still covers you | **Anyone who sees the paper owns that file** |

::: warning Three things you must know
1. **It is shown once.** We do not store it in the clear; miss it and you have to
   generate a new one (which retires the old).
2. **It is the file's weakest link.** Whoever holds that paper can open the file —
   there is no Argon2 in front of it, because none is needed.
3. **Opening with it does not reveal the file list.** Recovery codes are excluded
   from folder scans (that would keep them resident in memory), so "nothing
   happened" is expected — continue with `omy key restore` to set a new password.
:::

### If you mistype a word

Recovery codes carry a checksum and can point at **which word** looks wrong:

```
error: word 7 "acadmic" is not in the wordlist — did you mean "academic"?
```

The wordlist is the SLIP-39 English list: every word is 4–8 letters and **no two
words share their first four letters**, so copying just the first four is
unambiguous.

Swapping two words is invisible to a per-word check and is caught by the overall
checksum instead:

```
error: checksum mismatch: every word is valid, but the combination is not.
       Usually two words are in the wrong order, or one was copied as another word
```

A passing checksum does **not** mean the code belongs to this file — it only means
nothing was mistyped. A code from another vault is reported separately as "this
recovery code does not open that file". The two situations call for opposite
responses, so they are reported separately.

### Changing the password does not destroy the recovery code

`key change` and `key add` carry the file's other slots over untouched, so you can
keep changing passwords after setting a recovery code.

The one exception is `key remove`, whose whole meaning is "keep only the one I am
using now" — it retires every other password **including the recovery code**.

### If you do not want one

Simply never run `key recovery`. Then forgetting the password really does mean
permanent loss — which is not an oversight: any recovery mechanism is another
route around the password, which is to say another attack surface. Back your
passwords up yourself.
