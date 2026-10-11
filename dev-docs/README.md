# ZOL developer docs

ZOL (Zcash Operating Layer) lets an app accept **shielded ZEC**, run a real-time session in a
**MagicBlock rollup**, and pay the result back to **Zcash**. Players never handle SOL or a
Solana wallet, and nobody can link their in-app identity to their Zcash wallet.

These pages are for developers building apps on ZOL. Internal designs, plans and runbooks
live in [`../docs/`](../docs/).

## Start here

| Page | What it covers |
| --- | --- |
| [Overview](overview.md) | What ZOL is, what it guarantees, the moving parts |
| [Concepts](concepts.md) | Vault, deposits, balances, session keys, pots and seats, the rollup, exits |
| [Quickstart](quickstart.md) | A player key, a shielded deposit, coin flips in the rollup and a withdrawal, from code, in about 15 minutes |
| [Build an app](build-an-app.md) | The anatomy of a ZOL app, walked through on the coin flip |

## Reference

| Page | What it covers |
| --- | --- |
| [Ledger program](reference/ledger.md) | Every account, instruction, event and error of `zol-ledger` |
| [Gateway HTTP API](reference/gateway-api.md) | The operators' public API: app info, deposit intents, deposit status, the `ZOL1` memo |
| [TypeScript client](reference/typescript-client.md) | Instruction builders, decoders, session keys, passkeys, deposits |
| [Networks and addresses](reference/networks.md) | Program IDs, endpoints, the testnet deployment |

## Status

ZOL runs as a **testnet trial**: Zcash testnet (NU7), Solana devnet, MagicBlock's devnet
rollup, three operators on one host. Nothing here holds real money. The design is reviewed
in [`../docs/design-v0.3.1.md`](../docs/design-v0.3.1.md); the ledger has had internal review
only.
