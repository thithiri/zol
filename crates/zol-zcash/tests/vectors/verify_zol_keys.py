#!/usr/bin/env python3
"""Independent verifier for ZOL per-app key derivation vectors (zol_keys_v1.json).

Python standard library only: hashlib BLAKE2b, a BLAKE3 written here from the BLAKE3
specification (single-block inputs, which is all ZIP 2005's H^qk needs), and Pallas
arithmetic over Python integers. It shares no code with the Rust implementation.

Checked per case: every rejected counter (tweak, candidate ak and its ỹ = 1 or identity),
the accepted tweak and ak_app = ak_group + [t]G, the per-app secret sk_c, the ZIP 2005
use_qsk components (nk, qsk, qk, rivk_ext), the 96-byte FVK, the ZIP 32 internal rivk and
both outgoing viewing keys, the ZOL padding OVK and the session diversifier indices.
Diversified addresses (FF1 + Sinsemilla) are not recomputed here; the Rust side derives
them with the orchard crate.

Usage: python3 -I verify_zol_keys.py zol_keys_v1.json [zip2005_use_qsk_zafe.json]
"""

import hashlib
import json
import sys

P = 0x40000000000000000000000000000000224698FC094CF91B992D30ED00000001  # Pallas base field
Q = 0x40000000000000000000000000000000224698FC0994A8DD8C46EB2100000001  # Pallas scalar field
B = 5  # y^2 = x^3 + 5


# ---------------------------------------------------------------- hashes

def blake2b(personal: bytes, n: int, *parts: bytes) -> bytes:
    h = hashlib.blake2b(digest_size=n, person=personal)
    for p in parts:
        h.update(p)
    return h.digest()


def prf_expand(key: bytes, t: bytes) -> bytes:
    return blake2b(b"Zcash_ExpandSeed", 64, key, t)


IV = [0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A, 0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19]
MSG_PERM = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8]
CHUNK_START, CHUNK_END, ROOT, DERIVE_KEY_CONTEXT, DERIVE_KEY_MATERIAL = 1, 2, 8, 32, 64
MASK = 0xFFFFFFFF


def _g(v, a, b, c, d, x, y):
    v[a] = (v[a] + v[b] + x) & MASK
    v[d] = ((v[d] ^ v[a]) >> 16 | (v[d] ^ v[a]) << 16) & MASK
    v[c] = (v[c] + v[d]) & MASK
    v[b] = ((v[b] ^ v[c]) >> 12 | (v[b] ^ v[c]) << 20) & MASK
    v[a] = (v[a] + v[b] + y) & MASK
    v[d] = ((v[d] ^ v[a]) >> 8 | (v[d] ^ v[a]) << 24) & MASK
    v[c] = (v[c] + v[d]) & MASK
    v[b] = ((v[b] ^ v[c]) >> 7 | (v[b] ^ v[c]) << 25) & MASK


def _compress(cv, block: bytes, length: int, flags: int):
    m = [int.from_bytes(block[i:i + 4], "little") for i in range(0, 64, 4)]
    v = list(cv) + IV[:4] + [0, 0, length, flags]  # counter 0: single chunk
    for rnd in range(7):
        _g(v, 0, 4, 8, 12, m[0], m[1]); _g(v, 1, 5, 9, 13, m[2], m[3])
        _g(v, 2, 6, 10, 14, m[4], m[5]); _g(v, 3, 7, 11, 15, m[6], m[7])
        _g(v, 0, 5, 10, 15, m[8], m[9]); _g(v, 1, 6, 11, 12, m[10], m[11])
        _g(v, 2, 7, 8, 13, m[12], m[13]); _g(v, 3, 4, 9, 14, m[14], m[15])
        if rnd < 6:
            m = [m[i] for i in MSG_PERM]
    return [v[i] ^ v[i + 8] for i in range(8)]


def _blake3_one_block(key_words, data: bytes, flags: int) -> bytes:
    assert len(data) <= 64
    block = data + bytes(64 - len(data))
    out = _compress(key_words, block, len(data), flags | CHUNK_START | CHUNK_END | ROOT)
    return b"".join(w.to_bytes(4, "little") for w in out)


def blake3_derive_key(context: str, material: bytes) -> bytes:
    ctx_key = _blake3_one_block(IV, context.encode(), DERIVE_KEY_CONTEXT)
    words = [int.from_bytes(ctx_key[i:i + 4], "little") for i in range(0, 32, 4)]
    return _blake3_one_block(words, material, DERIVE_KEY_MATERIAL)


# ---------------------------------------------------------------- Pallas

def inv(a, m):
    return pow(a, m - 2, m)


def sqrt_p(a):
    """Tonelli-Shanks for P (P - 1 = 2^32 * t)."""
    a %= P
    if a == 0:
        return 0
    if pow(a, (P - 1) // 2, P) != 1:
        return None
    s, t = 0, P - 1
    while t % 2 == 0:
        s, t = s + 1, t // 2
    z = 5
    while pow(z, (P - 1) // 2, P) != P - 1:
        z += 1
    m, c, x, b = s, pow(z, t, P), pow(a, (t + 1) // 2, P), pow(a, t, P)
    while b != 1:
        i, b2 = 0, b
        while b2 != 1:
            b2, i = b2 * b2 % P, i + 1
        f = pow(c, 1 << (m - i - 1), P)
        m, c, x, b = i, f * f % P, x * f % P, b * f * f % P
    return x


def decode(b: bytes):
    if b == bytes(32):
        return None  # identity
    n = int.from_bytes(b, "little")
    ytilde, x = n >> 255, n & ((1 << 255) - 1)
    assert x < P
    y = sqrt_p(x * x * x + B)
    assert y is not None, "not on curve"
    if y & 1 != ytilde:
        y = P - y
    return (x, y)


def encode(pt) -> bytes:
    if pt is None:
        return bytes(32)
    x, y = pt
    return (x | ((y & 1) << 255)).to_bytes(32, "little")


def add(p1, p2):
    if p1 is None:
        return p2
    if p2 is None:
        return p1
    (x1, y1), (x2, y2) = p1, p2
    if x1 == x2:
        if (y1 + y2) % P == 0:
            return None
        lam = 3 * x1 * x1 * inv(2 * y1, P) % P
    else:
        lam = (y2 - y1) * inv(x2 - x1, P) % P
    x3 = (lam * lam - x1 - x2) % P
    return (x3, (lam * (x1 - x3) - y1) % P)


def mul(pt, k):
    r = None
    while k:
        if k & 1:
            r = add(r, pt)
        pt, k = add(pt, pt), k >> 1
    return r


def le(x: int) -> bytes:
    return x.to_bytes(32, "little")


# ---------------------------------------------------------------- ZOL v1

def derive(ak_group: bytes, K: bytes, app: bytes, net: int, G):
    d = bytes([1, net])
    enc = (32).to_bytes(2, "little") + app
    akg = decode(ak_group)
    rejected = []
    c = 0
    while True:
        cb = c.to_bytes(4, "little")
        t = int.from_bytes(blake2b(b"ZOL_app_ak_tweak", 64, d, ak_group, enc, cb), "little") % Q
        pt = add(akg, mul(G, t))
        akb = encode(pt)
        if pt is None or akb[31] >> 7 == 1:
            rejected.append({"c": c, "tweak": le(t).hex(), "ak_candidate": akb.hex()})
            c += 1
            continue
        sk = blake2b(b"ZOL_app_view_key", 32, d, K, enc, cb)
        nk = int.from_bytes(prf_expand(sk, b"\x07"), "little") % P
        qsk = prf_expand(sk, b"\x0c")[:32]
        qk = blake3_derive_key("Zcash ZIP 2005 qk-derivation v1", qsk)
        rivk = int.from_bytes(prf_expand(qk, b"\x0d" + akb + le(nk)), "little") % Q
        rivk_int = int.from_bytes(prf_expand(le(rivk), b"\x83" + akb + le(nk)), "little") % Q
        ovk = lambda r: prf_expand(le(r), b"\x82" + akb + le(nk))[32:].hex()
        fvk = akb + le(nk) + le(rivk)
        return {
            "counter": c, "rejected": rejected, "tweak": le(t).hex(), "ak_app": akb.hex(),
            "view_secret": sk.hex(), "nk": le(nk).hex(), "qsk": qsk.hex(), "qk": qk.hex(),
            "rivk_ext": le(rivk).hex(), "fvk": fvk.hex(), "ovk_external": ovk(rivk),
            "ovk_internal": ovk(rivk_int), "padding_ovk": blake2b(b"ZOL_padding_ovk1", 32, fvk).hex(),
        }


def check_zip2005(path):
    """ZIP 2005 use_qsk components of the third-party vectors (zip2005_use_qsk_zafe.json)."""
    v = json.load(open(path))
    for x in v["vectors"]:
        sk, ak = bytes.fromhex(x["sk"]), bytes.fromhex(x["ak"])
        nk = int.from_bytes(prf_expand(sk, b"\x07"), "little") % P
        qsk = prf_expand(sk, b"\x0c")[:32]
        qk = blake3_derive_key("Zcash ZIP 2005 qk-derivation v1", qsk)
        rivk = int.from_bytes(prf_expand(qk, b"\x0d" + ak + le(nk)), "little") % Q
        assert (le(nk).hex(), qsk.hex(), qk.hex(), le(rivk).hex()) == (x["nk"], x["qsk"], x["qk"], x["rivk_ext"])
        assert (ak + le(nk) + le(rivk)).hex() == x["fvk"]
    print(f"python verifier: {len(v['vectors'])} third-party ZIP 2005 vectors match (nk, qsk, qk, rivk_ext, FVK)")


def main(path):
    v = json.load(open(path))
    assert v["key_derivation_version"] == 1
    G = decode(bytes.fromhex(v["spend_auth_generator"]))
    ak_group, K = bytes.fromhex(v["ak_group"]), bytes.fromhex(v["master_view"])
    assert ak_group[31] >> 7 == 0
    n = 0
    for case in v["cases"]:
        app = bytes.fromhex(case["app_id"])
        got = derive(ak_group, K, app, case["network_byte"], G)
        for k, val in got.items():
            assert case[k] == val, f"{case['network']} {case['app_id'][:8]} {k}: {case[k]} != {val}"
        for s in case["sessions"]:
            idx = blake2b(b"ZOL_SessionRecv1", 32, app, bytes.fromhex(s["session_key"]))[:11]
            assert s["index"] == idx.hex()
        n += 1
    print(f"python verifier: {n} cases match (tweaks incl. {sum(len(c['rejected']) for c in v['cases'])} rejected counters, ak_app, sk_c, nk, qsk, qk, rivk_ext, FVK, OVKs, padding OVK, session indices)")


if __name__ == "__main__":
    main(sys.argv[1])
    if len(sys.argv) > 2:
        check_zip2005(sys.argv[2])
