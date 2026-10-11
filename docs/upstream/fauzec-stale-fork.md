# Draft note for the fauzec faucet operators: the faucet pays on the pre-NU7 fork

**Where to post:** fauzec's contact / issue page, or the Zcash community forum's testnet thread.

**Title:** fauzec testnet faucet is on the stale pre-NU7 chain

**Body:**

Testnet activated NU7 at height 4,465,026 on Oct 4, 2026. On Oct 10 a fauzec payout receipt
showed block 4,472,783, but that payment never appeared on the NU7 chain (our Zakura 1.6.0 and
Zebra 7 nodes were past that height since Oct 6 without it). The faucet's node appears to be
following the old, pre-NU7 fork, so its TAZ does not reach anyone on the current testnet.

Suggestion: upgrade the faucet's node to a release with NU7 (e.g. Zebra 7.0.0-rc.0 or
Zakura 1.6.0), and show the chain height on the faucet page so users can compare it with
their own node. Valar's faucet (faucet.testnet.valargroup.dev, `/api/status` →
`chainHeight`) is on the NU7 chain, for reference.
