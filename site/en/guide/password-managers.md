# Password-manager sync keys

omy can generate a high-entropy key and ask a third-party password manager to store it.
You do not need to remember or copy the key. When the same KDBX database is synced, the
credential can follow you to another device.

This complements, rather than replaces, the device key:

| | Password-manager sync key | Device key |
|---|---|---|
| Available after changing devices | Yes, after the KDBX is synced and unlocked | No |
| Stored by | KeePassXC, KeePassDX, or another provider | This machine's OS key store (Windows: TPM 2.0; macOS: Data Protection Keychain) |
| Daily action | Choose in the password manager | System biometric confirmation (Windows Hello / Touch ID) |
| Loss boundary | Losing the KDBX and its backups | Replacing the PC, OS, or resetting biometrics |

## KeePassXC on desktop

Enable **Browser Integration** in KeePassXC settings first. omy talks to KeePassXC through
the bundled `keepassxc-proxy` over a local encrypted channel. No browser window or browser
extension is required.

Standard installation directories are detected automatically. For a portable build, open
**Settings → Security → Portable KeePassXC** and enter the full path to
`keepassxc-proxy.exe` (for example, `E:\KeePassXC\keepassxc-proxy.exe`). If that explicit
path becomes invalid, omy reports the error instead of silently launching another binary
from `PATH` or a standard installation directory.

The first “Choose from password manager” action causes KeePassXC to show a new association
request. Give it a recognizable name such as `omy-my-laptop` and approve it. omy then only
queries entries for `https://credentials.omy.app/`. KeePassXC may separately ask whether a
particular entry may be returned the first time it is used.

Encryption offers two paths:

- **Choose an existing sync key** from matching KeePassXC entries.
- **Generate and save a new key**. omy generates 256 random bits, writes the entry, reads it
  back, and starts encryption only after that verification succeeds.

When KeePassXC is locked, omy can bring its unlock window to the front, but never receives
or enters the KDBX master password for you.

## Android and KeePassDX

Android currently uses KeePassDX Autofill or Magikeyboard on the ordinary password field.
omy does not show **Choose from password manager** or **Generate and save** there. After
syncing a KeePassXC sync-key entry to the phone, choose that same entry through KeePassDX on
the password field; the key still follows the existing Argon2id and key-slot path.

The explicit Android 14+ Credential Manager password API is disabled for now because a real
KeePassDX 4.5.5 round trip created entries containing only `AndroidApp=org.omy.app`, without
the paired protected `AndroidApp Signature` field. KeePassDX then requires both package name
and signature before returning the password, so the entry appears in the system picker but
always ends in `Password origin do not match`. The system also prioritizes that invalid
candidate, and the calling app cannot force KeePassDX to open its complete entry picker.

omy will not forge a web origin, bypass APK-signature checks, or rewrite protected fields in
a third-party KDBX. Explicit selection can be restored after providers reliably persist and
read back the signature field. Debug and release builds also have different signatures and
are treated as different apps by password managers.

## It is still stored as a password

The first implementation stores a generated sync key in a normal KeePass Password field;
it is not a passkey. Because the value is machine-generated rather than memorable, it has
full 256-bit entropy. Once received by omy it follows the existing Argon2id and key-slot
path, and it can still be supplied through the existing CLI password inputs.

This lets KeePassXC and KeePassDX share one secret after the one-time locator link above.
KeePassDX supports WebAuthn PRF, but KeePassXC currently supports passkey registration and
signatures without returning the stable PRF output needed for file encryption. omy can add
a real Passkey credential after both ends support it.

::: warning Writing the KDBX does not mean it has synced
Success means the entry reached the currently open password database. Uploading it is the
responsibility of your KDBX sync setup. After first using a new key, confirm synchronization
and retain a password or recovery code as a fallback.
:::

::: danger The password database becomes a security boundary
Anyone who can unlock the KDBX can obtain its omy sync keys. Use a strong database master
password and keep backups. Never use the repository fixture's `123456` password for real data.
:::
