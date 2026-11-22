#!/usr/bin/env python3
"""Minimal strict Parquet reader (uncompressed only) for verifying output.

Supports: flat schemas, required/optional columns, INT32/INT64/BYTE_ARRAY,
PLAIN, PLAIN_DICTIONARY/RLE_DICTIONARY, RLE def levels, data page v1 and v2.
Raises on anything else so a surprise is loud, not silent.
"""
import os
import struct
import sys


class R:
    def __init__(self, b, p=0):
        self.b, self.p = b, p

    def byte(self):
        v = self.b[self.p]
        self.p += 1
        return v

    def varint(self):
        s = v = 0
        while True:
            c = self.byte()
            v |= (c & 0x7F) << s
            s += 7
            if c < 0x80:
                return v

    def zz(self):
        v = self.varint()
        return (v >> 1) ^ -(v & 1)


def t_struct(r):
    """Decode a thrift compact struct into {field_id: value}."""
    out = {}
    fid = 0
    while True:
        h = r.byte()
        if h == 0:
            return out
        t = h & 0x0F
        d = h >> 4
        fid = fid + d if d else r.zz()
        out[fid] = t_val(r, t)


def t_val(r, t):
    if t == 1:
        return True
    if t == 2:
        return False
    if t == 3:
        return struct.unpack("b", bytes([r.byte()]))[0]
    if t in (4, 5, 6):
        return r.zz()
    if t == 7:
        v = struct.unpack("<d", r.b[r.p:r.p + 8])[0]
        r.p += 8
        return v
    if t == 8:
        n = r.varint()
        v = r.b[r.p:r.p + n]
        r.p += n
        return v
    if t in (9, 10):
        h = r.byte()
        n = h >> 4
        et = h & 0x0F
        if n == 15:
            n = r.varint()
        if et in (1, 2):  # bool elements in lists are 1 byte each
            return [r.byte() == 1 for _ in range(n)]
        return [t_val(r, et) for _ in range(n)]
    if t == 12:
        return t_struct(r)
    raise ValueError("thrift type %d" % t)


def hybrid(b, bw, n):
    """RLE / bit-packed hybrid decoding of n values."""
    r = R(b)
    out = []
    while len(out) < n:
        h = r.varint()
        if h & 1:
            groups = h >> 1
            m = (1 << bw) - 1
            for g in range(groups):
                chunk = int.from_bytes(b[r.p:r.p + bw], "little")
                r.p += bw
                out.extend((chunk >> (i * bw)) & m for i in range(8))
        else:
            cnt = h >> 1
            w = (bw + 7) // 8
            v = int.from_bytes(b[r.p:r.p + w], "little")
            r.p += w
            out.extend([v] * cnt)
    return out[:n], r.p


def plain(b, ptype, n):
    if ptype == 1:
        return list(struct.unpack("<%di" % n, b[:4 * n])), 4 * n
    if ptype == 2:
        return list(struct.unpack("<%dq" % n, b[:8 * n])), 8 * n
    if ptype == 6:
        out, p = [], 0
        for _ in range(n):
            ln = struct.unpack("<I", b[p:p + 4])[0]
            out.append(bytes(b[p + 4:p + 4 + ln]))
            p += 4 + ln
        return out, p
    raise ValueError("ptype %d" % ptype)


def read_file(path):
    b = open(path, "rb").read()
    assert b[:4] == b"PAR1" and b[-4:] == b"PAR1", "magic"
    flen = struct.unpack("<I", b[-8:-4])[0]
    meta = t_struct(R(b, len(b) - 8 - flen))
    schema = meta[2]
    root, cols = schema[0], schema[1:]
    assert root.get(5) == len(cols), "num_children"
    names = [c[4].decode() for c in cols]
    info = {}
    for c in cols:
        assert 5 not in c or c[5] == 0, "nested schema"
        info[c[4].decode()] = c
    total = meta[3]
    data = {n: [] for n in names}
    for rg in meta[4]:
        nrows = rg[3]
        for cc in rg[1]:
            md = cc[3]
            ptype = md[1]
            name = b".".join(md[3]).decode()
            assert md[4] == 0, "compressed codec %r" % md[4]
            optional = info[name].get(3, 0) == 1
            off = md.get(11, md[9])
            nvals = md[5]
            got = []
            dictv = None
            while len(got) < nvals:
                r = R(b, off)
                ph = t_struct(r)
                body = b[r.p:r.p + ph[3]]
                assert ph[2] == ph[3], "compressed page"
                off = r.p + ph[3]
                pt = ph[1]
                if pt == 2:  # dictionary
                    dictv, _ = plain(body, ptype, ph[7][1])
                    continue
                if pt == 0:
                    dh = ph[5]
                    n, enc = dh[1], dh[2]
                    p = 0
                    if optional:
                        ln = struct.unpack("<I", body[:4])[0]
                        defs, _ = hybrid(body[4:4 + ln], 1, n)
                        p = 4 + ln
                    else:
                        defs = [1] * n
                elif pt == 3:
                    dh = ph[8]
                    n, enc = dh[1], dh[4]
                    dl, rl = dh[5], dh[6]
                    assert rl == 0
                    if optional:
                        defs, _ = hybrid(body[:dl], 1, n)
                    else:
                        assert dl == 0
                        defs = [1] * n
                    p = dl
                else:
                    raise ValueError("page type %d" % pt)
                nn = sum(defs)
                rest = body[p:]
                if enc == 0:
                    vals, used = plain(rest, ptype, nn)
                    assert used == len(rest), "trailing bytes in plain page (%s)" % name
                elif enc in (2, 8):
                    assert dictv is not None
                    bw = rest[0]
                    idx, _ = hybrid(rest[1:], bw, nn) if nn else ([], 0)
                    vals = [dictv[i] for i in idx]
                else:
                    raise ValueError("encoding %d" % enc)
                it = iter(vals)
                got.extend(next(it) if d else None for d in defs)
            assert len(got) == nvals == nrows, "count mismatch in %s" % name
            data[name].extend(got)
    for n in names:
        assert len(data[n]) == total
    return names, info, data, total


def read_table(folder):
    files = sorted(
        f for f in os.listdir(folder)
        if f.endswith(".parquet") and not f.startswith((".", "_")) and os.path.isfile(os.path.join(folder, f)) and not os.path.islink(os.path.join(folder, f))
    )
    allrows, schema = [], None
    for f in files:
        names, info, data, total = read_file(os.path.join(folder, f))
        if schema is None:
            schema = (sorted(names), info)
        assert sorted(names) == schema[0], "schema differs across files"
        for i in range(total):
            allrows.append({n: data[n][i] for n in names})
    return files, schema, allrows


if __name__ == "__main__":
    files, schema, rows = read_table(sys.argv[1])
    print(len(files), "files", len(rows), "rows", schema[0])
    for n, c in schema[1].items():
        print(n, c)
    for r in rows[:5]:
        print(r)
