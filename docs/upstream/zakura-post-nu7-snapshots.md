# Draft issue for Zakura: 1.6.0 refuses testnet snapshots taken after NU7

**Where to post:** the Zakura repository's issue tracker (or Valar's snapshot channel).

**Title:** Zakura 1.6.0 crash-loops on testnet pruned snapshots taken after NU7 (NSM seed mismatch at 4,465,025)

**Body:**

Restoring Valar's testnet pruned snapshots (retention 10,000) published on Oct 7, 8 and 9, 2026
into Zakura 1.6.0 (state v29.1) crash-loops at startup on the database check:

```
NSM seed Amount(0) does not match the configured Amount(55768414957) at Height(4465025)
```

4,465,025 is the block before testnet's NU7 activation (4,465,026). It looks as if the
snapshot producer did not record the NU7 reissuance (NSM) seed the way 1.6.0 expects.

Workaround: restore the newest snapshot from before NU7,
`zakura-testnet-pruned-20261001T032907Z-4425493.tar.zst` (sha256
`b0db839fe0f0a359aee54c0b6a860e01869cf0c47d63177346d846388f6d58e4`), and let the node cross
NU7 itself; it records the seed and runs normally (about 60,000 blocks, ~75 minutes on a
2-vCPU host).

Environment: Zakura 1.6.0 (Docker image), Ubuntu 24.04, 2 vCPU / 2 GB + 4 GB swap. Happy to
provide logs. Worth checking the mainnet snapshots after mainnet's NU7 too.
