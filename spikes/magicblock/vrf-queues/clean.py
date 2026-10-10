#!/usr/bin/env python3
"""Writes empty copies of the local oracle's VRF queues.

@magicblock-labs/ephemeral-validator 0.14.10 ships its queue dumps full of stale requests
from another chain (53 items in the ephemeral queue, which is full). Their request slots are
far ahead of a local chain, so they can never be fulfilled or purged, and every new request
fails with AccountDataTooSmall. An empty queue has item_count 0, cursor 16 and its index
(ephemeral-vrf api/src/state/queue.rs); everything else is zero.
usage: clean.py <local-dumps dir> <out dir>"""
import base64, json, sys, os

src, out = sys.argv[1], sys.argv[2]
os.makedirs(out, exist_ok=True)
for key in ["Sc9MJUngNbQXSXGP3F67KvKwVnhaYn6kcioxXNVowYT", "GKE6d7iv8kCBrsxr78W3xVdjGLLLJnxsGiuzrsZCGEvb"]:
    d = json.load(open(f"{src}/{key}.json"))
    b = bytearray(base64.b64decode(d["account"]["data"][0]))
    index, paused = b[16], b[17]
    clean = bytearray(len(b))
    clean[0:8] = b[0:8]                      # account discriminator
    clean[8:12] = (0).to_bytes(4, "little")  # item_count
    clean[12:16] = (16).to_bytes(4, "little")  # cursor
    clean[16] = index
    clean[17] = paused
    d["account"]["data"][0] = base64.b64encode(bytes(clean)).decode()
    json.dump(d, open(f"{out}/{key}.json", "w"))
    print(key, "items", int.from_bytes(b[8:12], "little"), "-> 0; index", index)
