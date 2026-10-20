#!/usr/bin/env python3
"""Independent Murmur3 x64/128 seed hypothesis probe, not an SPP checksum claim.

Algorithm reference: Austin Appleby's public-domain SMHasher MurmurHash3.cpp
https://github.com/aappleby/smhasher/blob/master/src/MurmurHash3.cpp
Reversing the mixing arithmetic avoids a seed brute-force search. Equal inferred
initial states across different datasets are required before proposing a seed.
"""
import struct

MASK = (1 << 64) - 1
C1, C2 = 0x87C37B91114253D5, 0x4CF5AD432745937F
F1, F2 = 0xFF51AFD7ED558CCD, 0xC4CEB9FE1A85EC53


def rot(v, n):
    n %= 64
    return ((v << n) | (v >> ((64 - n) % 64))) & MASK


def mix(v, c1, n, c2):
    return (rot((v * c1) & MASK, n) * c2) & MASK


def fmix(v):
    v ^= v >> 33
    v = (v * F1) & MASK
    v ^= v >> 33
    v = (v * F2) & MASK
    return v ^ (v >> 33)


def unmix(v):
    v ^= v >> 33
    v = (v * pow(F2, -1, 1 << 64)) & MASK
    v ^= v >> 33
    v = (v * pow(F1, -1, 1 << 64)) & MASK
    return v ^ (v >> 33)


def tail_keys(data):
    tail = data[len(data) // 16 * 16:]
    k1 = int.from_bytes(tail[:8], 'little')
    k2 = int.from_bytes(tail[8:], 'little')
    return mix(k1, C1, 31, C2), mix(k2, C2, 33, C1)


def digest(data, initial=(0, 0)):
    h1, h2 = initial
    for offset in range(0, len(data) // 16 * 16, 16):
        k1, k2 = struct.unpack_from('<QQ', data, offset)
        h1 ^= mix(k1, C1, 31, C2)
        h1 = ((rot(h1, 27) + h2) * 5 + 0x52DCE729) & MASK
        h2 ^= mix(k2, C2, 33, C1)
        h2 = ((rot(h2, 31) + h1) * 5 + 0x38495AB5) & MASK
    k1, k2 = tail_keys(data)
    h1 ^= k1 ^ len(data)
    h2 ^= k2 ^ len(data)
    h1 = (h1 + h2) & MASK
    h2 = (h2 + h1) & MASK
    h1, h2 = fmix(h1), fmix(h2)
    h1 = (h1 + h2) & MASK
    h2 = (h2 + h1) & MASK
    return h1, h2


def infer_initial(data, result):
    """Invert the standard algorithm for a known data/digest pair."""
    h1, h2 = result
    h2 = (h2 - h1) & MASK
    h1 = (h1 - h2) & MASK
    h1, h2 = unmix(h1), unmix(h2)
    h2 = (h2 - h1) & MASK
    h1 = (h1 - h2) & MASK
    k1, k2 = tail_keys(data)
    h1 ^= len(data) ^ k1
    h2 ^= len(data) ^ k2
    inverse5 = pow(5, -1, 1 << 64)
    for offset in reversed(range(0, len(data) // 16 * 16, 16)):
        k1, k2 = struct.unpack_from('<QQ', data, offset)
        h2 = rot((((h2 - 0x38495AB5) * inverse5) - h1) & MASK, -31)
        h2 ^= mix(k2, C2, 33, C1)
        h1 = rot((((h1 - 0x52DCE729) * inverse5) - h2) & MASK, -27)
        h1 ^= mix(k1, C1, 31, C2)
    return h1, h2
