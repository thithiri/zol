# ZOL developer docs

ZOL (Zcash Operating Layer) is **programmable Zcash privacy on Solana's SVM**. Apps are
Solana programs: they accept **shielded ZEC**, run real-time sessions in a **MagicBlock/SVM
rollup**, and pay the result back to **Zcash**. Players never handle SOL or a Solana wallet,
and nothing public links their in-app identity to their Zcash wallet. New to the terms? See
[Concepts](concepts.md) or the [glossary](faq.md#glossary).

These pages are for developers building apps on ZOL. Internal designs, plans and runbooks
live in [`../docs/`](../docs/).

## Start here

| Page | What it covers |
| --- | --- |
| [Overview](overview.md) | What ZOL is, what it guarantees, the moving parts |
| [Concepts](concepts.md) | Vault, deposits, balances, session keys, pots and seats, the rollup, exits |
| [Quickstart](quickstart.md) | A player key, a shielded deposit, coin flips in the rollup and a withdrawal, from code, in about 15 minutes |

## Build

| Page | What it covers |
| --- | --- |
| [Build an app](build-an-app.md) | The anatomy of a ZOL app, walked through on the coin flip |
| [Example apps](examples.md) | Coin flip (VRF), ZEC Up/Down (price oracle), sealed-bid auction (commit-reveal) |
| [App design patterns](patterns.md) | Sessions, a counterparty service, closing without a keeper, deciding outcomes, many players |
| [Session keys and passkeys](session-keys.md) | The player's key: what it signs, passkey derivation, backups, privacy |

## Reference

| Page | What it covers |
| --- | --- |
| [Ledger program](reference/ledger.md) | Every account, instruction, event and error of `zol-ledger` |
| [Gateway HTTP API](reference/gateway-api.md) | The operators' public API: app info, deposit intents, deposit status, the `ZOL1` memo |
| [TypeScript client](reference/typescript-client.md) | Instruction builders, decoders, session keys, passkeys, deposits |
| [Networks and addresses](reference/networks.md) | Program IDs, endpoints, testnet settings, MagicBlock in ZOL |

## Ship

| Page | What it covers |
| --- | --- |
| [Security model](security.md) | Who can do what, what the ledger guarantees, an app review checklist, known limitations |
| [Testing](testing.md) | LiteSVM, the local stack, devnet scripts |
| [Deploying](deploying.md) | From build to an active app on the testnet, services and pages |
| [FAQ, glossary, troubleshooting](faq.md) | Common questions, terms, errors and their causes |

## Status

ZOL runs as a **fully functional beta on testnet**: Zcash testnet (NU7), Solana devnet,
MagicBlock's devnet rollup, three operators on one host. Players use testnet ZEC (TAZ). The
live demo is at [`/play.html`](/play.html). The design is reviewed
in [`../docs/design-v0.3.1.md`](../docs/design-v0.3.1.md); the ledger has had internal review
only.
