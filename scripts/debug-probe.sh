#!/usr/bin/env bash
# Probe what a debugger can do with a Tinox `--debug` build today (issue #497).
# Usage: scripts/debug-probe.sh [path/to/tinox]
# Needs gdb. Builds a small sample in a temp dir, then reports DWARF coverage and
# what breakpoints / stepping / locals do. No output is asserted: read the report.
set -euo pipefail
TINOX=$(realpath "${1:-$(dirname "$0")/../target/debug/tinox}")
command -v gdb >/dev/null || { echo "gdb not found"; exit 1; }
DIR=$(mktemp -d /tmp/tinox_debug_probe_XXXXXX)
trap 'rm -rf -- "$DIR"' EXIT
cd "$DIR"
VERSION=$("$TINOX" --version | awk '{print $2}')
printf '[package]\ncompiler = "%s"\nname = "probe"\nversion = "0.1.0"\n' "$VERSION" > tinox.toml
cat > Main.tnx <<'TNX'
class Main
{
    fnc work(n: Int64) -> Int64
    {
        var total: Int64 = 0
        var i: Int64 = 0
        while (i < n)
        {
            total = total + i
            i = i + 1
        }
        return total
    }

    fnc main() -> Int32
    {
        const sum: Int64 = Main::work(5)
        println(sum.toString())
        return 0
    }
}
TNX
"$TINOX" build Main.tnx --debug -o probe >/dev/null

echo "== DWARF contents (tags / named entities / line-table rows)"
readelf --debug-dump=info probe | grep -oE 'DW_TAG_[a-z_]+' | sort | uniq -c
readelf --debug-dump=decodedline probe | grep -c '^Main.tnx' || true

cat > cmds.gdb <<'GDB'
set pagination off
set debuginfod enabled off
break Main.work
break Main.tnx:10
run
bt 2
info locals
info args
next
GDB
echo "== gdb: breakpoint by function, by line, backtrace, locals, step"
gdb -q -batch -x cmds.gdb ./probe 2>&1 | grep -vE '^\[(New|Thread)|libthread_db|Using host' || true

echo "== gdb DAP (gdb --interpreter=dap)"
python3 - <<'PY'
import json, os, select, subprocess, time
p = subprocess.Popen(["gdb", "-q", "--interpreter=dap"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
def send(seq, cmd, args=None):
    body = json.dumps({"seq": seq, "type": "request", "command": cmd, "arguments": args or {}}).encode()
    p.stdin.write(b"Content-Length: %d\r\n\r\n%s" % (len(body), body)); p.stdin.flush()
def read(timeout=5):
    end, buf = time.time() + timeout, b""
    while b"\r\n\r\n" not in buf:
        if time.time() > end: return None
        if select.select([p.stdout], [], [], 0.5)[0]: buf += os.read(p.stdout.fileno(), 1)
    n = int([l for l in buf.split(b"\r\n") if l.lower().startswith(b"content-length")][0].split(b":")[1])
    body = b""
    while len(body) < n: body += os.read(p.stdout.fileno(), n - len(body))
    return json.loads(body)
send(1, "initialize", {"adapterID": "tinox"})
r = read(); print("initialize success:", r and r.get("success"))
send(2, "launch", {"program": os.path.abspath("probe")})
# DAP order: breakpoints are set after the adapter's `initialized` event
for _ in range(20):
    m = read(5)
    if m and m.get("event") == "initialized": break
send(3, "setFunctionBreakpoints", {"breakpoints": [{"name": "Main.work"}]})
send(4, "configurationDone")
stopped = None
for _ in range(60):
    m = read(5)
    if not m: break
    if m.get("event") == "stopped": stopped = m["body"]; break
    if m.get("command") == "setFunctionBreakpoints": print("breakpoint:", m["body"]["breakpoints"])
print("stopped event:", stopped and stopped.get("reason"))
p.kill()
PY
