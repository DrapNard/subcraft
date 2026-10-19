#!/usr/bin/env python3
"""Bounded clean-room schema probe for owned DataDocument extracts only.

This scans for self-describing Data* class declarations; it deliberately does
not deserialize resource payloads or guess widths for unknown property codes.
"""
from __future__ import annotations
import argparse, json, struct
from dataclasses import dataclass
from pathlib import Path

MAX_BYTES = 64 * 1024
MAX_CLASSES = 256
MAX_PROPERTIES = 128
MAX_NAME_BYTES = 128
MAX_DEPTH = 32  # reserved for the future object-reference walker
KNOWN = {1: "f32?", 9: "u32?", 10: "u8?", 12: "u64?", 16: "string?", 17: "array?", 18: "object-ref?", 19: "object-array?", 20: "embedded-object?"}

@dataclass(frozen=True)
class Property:
    name: str
    type_code: int

@dataclass(frozen=True)
class ClassDef:
    offset: int
    end: int
    name: str
    props: tuple[Property, ...]

def u32(b: bytes, off: int) -> int:
    if off < 0 or off + 4 > len(b):
        raise ValueError("truncated u32")
    return struct.unpack_from("<I", b, off)[0]

def parse_candidate(b: bytes, off: int) -> ClassDef | None:
    # Class marker: FFFFFFFF, then bounded length-prefixed ASCII class name,
    # property count, then repeated (name length, name bytes, type code).
    try:
        if b[off:off+4] != b"\xff" * 4: return None
        n = u32(b, off+4)
        if not 1 <= n <= MAX_NAME_BYTES: return None
        name = b[off+8:off+8+n].decode("ascii")
        if not name.startswith("Data") or not name.replace("_", "").isalnum(): return None
        cur = off+8+n
        count = u32(b, cur); cur += 4
        if count > MAX_PROPERTIES: return None
        props = []
        for _ in range(count):
            size = u32(b, cur); cur += 4
            if not 1 <= size <= MAX_NAME_BYTES or cur+size+4 > len(b): return None
            key = b[cur:cur+size].decode("ascii"); cur += size
            if not key.replace("_", "").isalnum(): return None
            code = u32(b, cur); cur += 4
            props.append(Property(key, code))
        return ClassDef(off, cur, name, tuple(props))
    except (ValueError, UnicodeDecodeError, struct.error):
        return None

def inspect(path: Path) -> dict:
    with path.open("rb") as f:
        b = f.read(MAX_BYTES + 1)
    if len(b) > MAX_BYTES: raise ValueError(f"{path.name}: exceeds {MAX_BYTES} bytes")
    if len(b) < 24 or b[:4] != bytes.fromhex("dd 2f 7c 1b"):
        raise ValueError(f"{path.name}: unrecognized DataDocument header")
    version, word2, word3, word4, declared = struct.unpack_from("<IIIII", b, 4)
    if version != 1 or declared != len(b):
        raise ValueError(f"{path.name}: version/declared-size mismatch")
    classes = []
    i = 0
    while i <= len(b)-16:
        if b[i:i+4] == b"\xff"*4:
            c = parse_candidate(b, i)
            if c is not None:
                # Keep the first declaration for each class name; duplicate byte
                # patterns may occur in the serialized object graph.
                old = next((old for old in classes if old.name == c.name), None)
                if old is not None and old.props != c.props:
                    raise ValueError(f"{path.name}: conflicting declarations for {c.name}")
                if old is None:
                    if len(classes) >= MAX_CLASSES:
                        raise ValueError("class count exceeds research limit")
                    classes.append(c)
                i = c.end
                continue
        i += 1
    if not classes:
        raise ValueError("no supported class declarations found")
    seen: dict[str, tuple[Property,...]] = {}
    for c in classes:
        prev = seen.get(c.name)
        if prev is not None and prev != c.props:
            raise ValueError(f"{path.name}: conflicting declarations for {c.name}")
        seen[c.name] = c.props
    return {"file": path.name, "bytes": len(b), "header": {"version":version,"word2":word2,"word3":word3,"word4":word4,"declared_size":declared}, "classes": classes, "_data": b}

def main() -> None:
    ap=argparse.ArgumentParser()
    ap.add_argument("files", nargs="+", type=Path)
    args=ap.parse_args()
    parsed=[inspect(p) for p in args.files]
    names=[{c.name: c for c in item["classes"]} for item in parsed]
    canonical=names[0]
    for item, table in zip(parsed, names):
        print(f"{item['file']}: {item['bytes']} bytes; header={item['header']}; discovered {len(table)} class declarations")
        for c in item["classes"]:
            fields=", ".join(f"{p.name}: {p.type_code} ({KNOWN.get(p.type_code, 'UNRESOLVED')})" for p in c.props)
            print(f"  0x{c.offset:04x} {c.name}: {fields}")
        for name, c in table.items():
            if name in canonical and c.props != canonical[name].props:
                raise SystemExit(f"schema drift for {name} in {item['file']}")
    # Differential evidence: locate the known changed label as length-prefixed
    # UTF-8 and report only its byte span. This is a candidate field-value match,
    # not a proof of a full object walk.
    for item, path in zip(parsed, args.files):
        b=item["_data"]
        for text in ("Calque 1", "Kalium01"):
            raw=text.encode("utf-8")
            off=b.find(raw)
            if off >= 4 and off >= 0 and u32(b, off-4) == len(raw):
                print(f"  candidate string value {text!r} at 0x{off:04x} (u32 length prefix)")
        for f in (1.0, 0.375):
            raw=struct.pack("<f",f); offsets=[]; start=0
            while len(offsets)<16:
                off=b.find(raw,start)
                if off<0:break
                offsets.append(off);start=off+1
            if offsets: print(f"  f32 bit-pattern {f}: candidate offsets {[hex(x) for x in offsets]}")
    print("LIMIT: this prototype recognizes declaration grammar and candidate scalar encodings only; it does not follow objects, verify pointer ownership/aliases, or write documents.")

if __name__ == "__main__": main()
