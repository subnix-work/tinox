#!/usr/bin/env python3
"""Set `compiler = "<version>"` in every [package] section (issue #449).

Covers tinox.toml files and manifests embedded in Rust/Python/shell test
fixtures. Run after bumping the workspace version (default: workspace version).
"""
import re, subprocess, sys, os

root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
os.chdir(root)
ver = sys.argv[1] if len(sys.argv) > 1 else re.search(r'^version = "(.*)"', open("Cargo.toml").read(), re.M).group(1)
files = subprocess.check_output(
    "git ls-files -co --exclude-standard | grep -E '(^|/)tinox\\.toml$|\\.(rs|py|sh)$'",
    shell=True, text=True).split()
SKIP = {"scripts/set_compiler_version.py", "crates/tinox/src/semicolon_migration.rs", "crates/tinox/tests/compiler_version.rs"}
for f in files:
    if f in SKIP or "/.tinox/" in f or not os.path.isfile(f):
        continue
    s = o = open(f).read()
    if f.endswith("tinox.toml"):
        if re.search(r"^\[package\]", s, re.M):
            if re.search(r"^compiler\s*=", s, re.M):
                s = re.sub(r"^compiler\s*=.*$", f'compiler = "{ver}"', s, flags=re.M)
            else:
                s = re.sub(r"^\[package\][ \t]*\n", f'[package]\ncompiler = "{ver}"\n', s, count=1, flags=re.M)
    else:
        # one-line string literals: "[package]\nname = ..."
        s = re.sub(r'compiler = \\"[0-9][0-9.]*\\"', lambda m: f'compiler = \\"{ver}\\"', s)
        s = re.sub(r'\[package\]\\n(?!compiler = )', lambda m: f'[package]\\ncompiler = \\"{ver}\\"\\n', s)
        # multi-line literals / heredocs
        s = re.sub(r'(?m)^([ \t]*)compiler = "[0-9][0-9.]*"$', lambda m: f'{m.group(1)}compiler = "{ver}"', s)
        s = re.sub(r'\[package\]\n(?![ \t]*compiler = )([ \t]*)', lambda m: f'[package]\n{m.group(1)}compiler = "{ver}"\n{m.group(1)}', s)
    if s != o:
        open(f, "w").write(s)
