#!/usr/bin/env python3
"""Held-out checks. Run against a disposable snapshot, never model-written tests."""
import http.client
import json
import os
import re
from pathlib import Path
import socket
import subprocess
import sys
import time


def build(work, tinox, env=None):
    subprocess.run([tinox, "build", str(work / "Main.tnx"), "-o", str(work / "accepted")],
                   cwd=work, env=env, check=True, stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL, timeout=90)


def bugfix(work, tinox):
    source = (work / "Main.tnx").read_text()
    # Keep the submitted implementation, replace only its smoke entry point.
    match = re.search(r"\bfnc\s+main\s*\(\s*\)", source)
    if match is None:
        raise ValueError("missing Main entry point")
    start = match.start()
    brace = source.index("{", start)
    depth, end = 1, brace + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    cases = [(199, 3, 15), (0, 4, 0), (37, 0, 70), (999, 2, 100), (13, 1, 1),
             (12, 4, 0), (-1, 2, 0), (2, -1, 0), (2, 4, -1), (2, 4, 101),
             (1000000, 1000, 33)]
    main = 'fnc main() -> Int32 {\n' + ''.join(
        f'print(total({p}, {q}, {d}))\nprint("\\n")\n' for p, q, d in cases) + 'return 0\n}'
    (work / "Main.tnx").write_text(source[:start] + main + source[end:])
    build(work, tinox)
    result = subprocess.run([str(work / "accepted")], capture_output=True, text=True, timeout=5, check=True)
    expected = [str(-1 if p < 0 or q < 0 or not 0 <= d <= 100 else p * q * (100-d) // 100)
                for p, q, d in cases]
    assert result.stdout.splitlines() == expected


def http_check(task, work, tinox):
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    env = dict(os.environ, TINOX_PORT=str(port))
    build(work, tinox, env)
    service = subprocess.Popen([str(work / "accepted")], cwd=work,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def call(method, path, expected, body=None):
        conn = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
        try:
            conn.request(method, path, None if body is None else json.dumps(body),
                         {"Content-Type": "application/json"})
            response = conn.getresponse()
            raw = response.read()
            assert response.status == expected
            if expected == 204:
                assert raw == b""
                return None
            assert "application/json" in response.getheader("Content-Type", "")
            return json.loads(raw)
        finally:
            conn.close()

    try:
        deadline = time.monotonic() + 8
        while True:
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=.2):
                    break
            except OSError:
                if service.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError("service not ready")
                time.sleep(.05)
        if task == "validation":
            assert call("GET", "/users/1", 200) == {"id": 1, "name": "Alice"}
            for user_id in (2, 39, 90001):
                assert call("GET", f"/users/{user_id}", 404) == {"error": "not found"}
            for user_id in (0, -1, -912):
                assert call("GET", f"/users/{user_id}", 400) == {"error": "invalid id"}
        else:
            assert call("GET", "/users", 200) == []
            alice = call("POST", "/users", 201, {"name": "Ada"})
            bob = call("POST", "/users", 201, {"name": "Grace"})
            assert alice == {"id": 1, "name": "Ada"}
            assert bob == {"id": 2, "name": "Grace"}
            assert call("GET", "/users", 200) == [alice, bob]
            assert call("GET", "/users/2", 200) == bob
            changed = {"id": 1, "name": "Updated Name"}
            assert call("PUT", "/users/1", 200, {"name": changed["name"]}) == changed
            assert call("GET", "/users/1", 200) == changed
            call("DELETE", "/users/1", 204)
            for method in ("GET", "PUT", "DELETE"):
                assert call(method, "/users/1", 404, {"name": "Ghost"} if method == "PUT" else None) == {"error": "not found"}
            assert call("GET", "/users", 200) == [bob]
            assert call("POST", "/users", 201, {"name": "Linus"}) == {"id": 3, "name": "Linus"}
    finally:
        try:
            service.kill()
        except ProcessLookupError:
            pass
        service.wait()


if __name__ == "__main__":
    task, directory, compiler = sys.argv[1:]
    work = Path(directory)
    if task == "bugfix":
        bugfix(work, compiler)
    elif task in ("validation", "crud"):
        http_check(task, work, compiler)
    else:
        raise ValueError("unknown task")
