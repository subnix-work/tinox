#!/usr/bin/env python3
"""Regenerates the Unicode case-mapping tables embedded in runtime/runtime.c.

The tables are the default (locale-independent, context-free) full case mappings of
the Unicode version of the running Python (`unicodedata.unidata_version`), taken from
`str.upper()` / `str.lower()` applied to each single scalar value. Run it with
`--write` to rewrite the block between the BEGIN/END markers; without it the block is
printed. `scripts/check-unicode.py` compares the compiled module against Python.
"""
import re
import sys
import unicodedata

BEGIN = "// BEGIN GENERATED UNICODE CASE TABLES"
END = "// END GENERATED UNICODE CASE TABLES"


def table(name, fn):
    pool, entries = [], []
    for cp in range(0x110000):
        if 0xD800 <= cp <= 0xDFFF:
            continue
        mapped = [ord(c) for c in fn(chr(cp))]
        if mapped != [cp]:
            entries.append((cp, len(pool), len(mapped)))
            pool.extend(mapped)
    out = [f"static const uint32_t tinox_unicode_{name}_pool[] = {{"]
    out += ["    " + ", ".join(str(v) for v in pool[i:i + 12]) + "," for i in range(0, len(pool), 12)]
    out.append("};")
    out.append(f"static const struct {{ uint32_t cp; uint16_t offset; uint8_t len; }} tinox_unicode_{name}_map[] = {{")
    out += ["    " + " ".join("{%d,%d,%d}," % e for e in entries[i:i + 6]) for i in range(0, len(entries), 6)]
    out.append("};")
    return "\n".join(out)


def block():
    version = unicodedata.unidata_version
    return "\n".join([
        BEGIN + f" (Unicode {version}, scripts/gen-unicode-case.py)",
        f'#define TINOX_UNICODE_VERSION "{version}"',
        table("upper", str.upper),
        table("lower", str.lower),
        END,
    ])


if __name__ == "__main__":
    text = block()
    if "--write" in sys.argv:
        path = "runtime/runtime.c"
        src = open(path, encoding="utf-8").read()
        pattern = re.compile(re.escape(BEGIN) + r".*?" + re.escape(END), re.S)
        if not pattern.search(src):
            sys.exit("markers not found in " + path)
        open(path, "w", encoding="utf-8").write(pattern.sub(lambda _: text, src, count=1))
    else:
        print(text)
