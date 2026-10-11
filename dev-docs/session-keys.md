# Session keys and passkeys

A ZOL player **is** an Ed25519 session key: one per app. The ledger knows the player only by
this key; there is no account, email or Solana wallet behind it.

## What the key signs

| Where | What |
| --- | --- |
| Zcash deposit | The deposit memo (the session key, payout address, vault, network, expiry, flags): the operators credit only a memo the key signed |
| Solana | `buy_in` (into a pot, with the app), `request_exit`, `leave_pot` |
| Rollup | App actions such as `flip`, `bet`, `commit_bid`, `reveal_bid`: free, the key is its own fee payer there |

It never needs SOL: on Solana an app service pays the fees and co-signs.

## What a stolen key can and cannot do

- It **can** play the balance away in the app's games, and request an exit.
- It **cannot** send the balance anywhere else: payouts go only to the payout address fixed at
  the player's first deposit, which the ledger holds as a commitment and the operators check
  before signing. A deposit naming a different payout address for the same key is voided.
- A lost key is a lost balance: the ledger has no recovery. Keep it safe, or derive it from a
  passkey.

## Where to keep it

### A passkey (recommended for pages)

WebAuthn's PRF extension gives a secret that is fixed for a passkey and a salt. ZOL derives
the session seed from it, so the same passkey gives the same key on every device it syncs to,
and there is nothing to write down.

```text
salt = SHA-256("zol/session/v1/" ‖ app)
seed = SHA-256("ZOL passkey session v1" ‖ app ‖ PRF(passkey, salt))
```

- Create: `createPasskeySeed(app)` (a resident passkey with user verification), then the seed.
  Sign in later: `seedFromPasskey(app)`. See
  [`../web/src/passkey.ts`](../web/src/passkey.ts).
- Support depends on **where the passkey is saved**: Google Password Manager (Chrome 132+,
  signed in), iCloud Keychain (Safari 18+, iOS 18+), Android, a phone via QR code, 1Password.
  Chrome's local profile and some managers (e.g. Opera's) have no PRF: `prfSupported()` asks
  the browser first, and creation fails with advice if PRF is missing.
- Trust: whoever controls the synced passkey (the player, their password manager's account)
  controls the key.

### A saved session key

A random 32-byte seed kept in the browser (`localStorage` key `zol:session:<app>`). The
player must save a copy:

```text
zolsess1:<app program>:<seed hex>:<checksum>
```

The checksum is the first 4 bytes of SHA-256 over everything before it, so a mistyped or
truncated copy is caught. `exportSession` / `restoreSession` in
[`../web/src/session.ts`](../web/src/session.ts). The testnet pages refuse a deposit until the
key has been shown once.

### A file (scripts, services)

The [quickstart](quickstart.md) keeps the seed in a JSON file (mode 600). A service such as the
house keeps its seed the same way, outside the repository.

## Privacy

- Solana sees the key's actions; Zcash sees only shielded payments into the vault and out to
  the payout address. Nothing links the two.
- One key per app: the same player in two apps is two unrelated keys.
- A player who wants separate histories can use a fresh key (and passkey) per session; each
  key's payout address is fixed at its own first deposit.
