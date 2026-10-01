# Password-manager sync keys

omy can generate a high-entropy key and ask a third-party password manager to store it.
You do not need to remember or copy the key. When the same KDBX database is synced, the
credential can follow you to another device.

This complements, rather than replaces, the Windows Hello device key:

| | Password-manager sync key | Windows Hello device key |
|---|---|---|
| Available after changing devices | Yes, after the KDBX is synced and unlocked | No |
| Stored by | KeePassXC, KeePassDX, or another provider | This machine's TPM |
| Daily action | Choose in the password manager | Confirm Windows Hello |
| Loss boundary | Losing the KDBX and its backups | Replacing the PC, OS, TPM, or Hello setup |

## KeePassXC on desktop

Enable **Browser Integration** in KeePassXC settings first. omy talks to KeePassXC through
the bundled `keepassxc-proxy` over a local encrypted channel. No browser window or browser
extension is required.

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

On Android 14 and later, omy uses the system Credential Manager. The system picker can be
served by KeePassDX, Google Password Manager, or another enabled provider, both when
choosing and when saving a generated key.

Android 13 and earlier cannot use third-party Credential Providers. The existing input
fields continue to support Autofill and KeePassDX's Magikeyboard as a fallback.

The two clients use different matching metadata in one KDBX entry: KeePassXC matches the
URL, while KeePassDX records `org.omy.app` and the app signing certificate. Standard APIs
do not let an ordinary app write another platform's origin metadata, so syncing the KDBX
syncs the secret value but does not automatically add the other locator.

- After creating on Android, add `URL=https://credentials.omy.app/` to that same entry in
  KeePassDX before syncing it to the desktop.
- After creating on the desktop, use KeePassDX Autofill on the ordinary password field the
  first time on Android. Select the same entry and allow KeePassDX to save the app link.
  Android 14+'s system picker can find it on subsequent uses.
- Do not select an existing desktop entry from Android's **Generate and save** flow merely
  to link it. That request carries a newly generated secret and would replace the old
  password, leaving files encrypted with the old value without that recovery path.

This is an API boundary: Android prevents an app from impersonating a website, while the
KeePassXC-Browser protocol cannot write arbitrary KDBX custom fields. It is not a KDBX sync
failure. Debug and release builds also have different signatures and are treated as
different apps.

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
