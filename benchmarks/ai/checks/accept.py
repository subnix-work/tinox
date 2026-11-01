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


def replace_main(work, main):
    source = (work / "Main.tnx").read_text()
    match = re.search(r"\bfnc\s+main\s*\(\s*\)", source)
    if match is None:
        raise ValueError("missing Main entry point")
    brace = source.index("{", match.start())
    depth, end = 1, brace + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    (work / "Main.tnx").write_text(source[:match.start()] + main + source[end:])


def concurrent(work, tinox):
    import shutil
    checker = Path(__file__).resolve().parent
    shutil.copyfile(checker / "concurrent_job.c", work / "eval_job.c")
    # Native instrumentation is held out; model-written Job code is replaced.
    (work / "Job.tnx").write_text('''extern fn evalWork(value: Int64) -> Int64
extern fn evalActive() -> Int64
extern fn evalPeak() -> Int64
extern fn evalCalls() -> Int64
extern fn evalReset() -> Nothing
class Job {
 fnc run(value: Int64) -> Int64 {
  if (value < 0) { throw "invalid job" }
  return evalWork(value)
 }
}
''')
    (work / "tinox.toml").write_text('[package]\ncompiler="2.8.0"\nname="concurrent_accept"\nversion="0.0.0"\n[native]\nsources=["eval_job.c"]\n')
    cases = [([], 3), ([7], 1), ([1, 2, 3, 4, 5], 2), (list(range(1, 18)), 4), ([9, 2, 9], 9), ([1, 2], 0), ([1, -1, 3, 4], 3)]
    body = []
    for values, limit in cases:
        body.append('evalReset()')
        body.append('try { println(Main::parallelChecksum(' + str(values) + ', ' + str(limit) + ')) } catch (error: String) { println("error") }')
        body.append('println(evalActive().toString() + ":" + evalPeak().toString() + ":" + evalCalls().toString())')
    replace_main(work, 'fnc main() -> Int32 {\n' + '\n'.join(body) + '\nreturn 0\n}')
    build(work, tinox)
    result = subprocess.run([work / "accepted"], capture_output=True, text=True, check=True, timeout=10)
    lines = result.stdout.splitlines()
    assert len(lines) == len(cases) * 2
    for i, (values, limit) in enumerate(cases):
        active, peak, calls = map(int, lines[i * 2 + 1].split(':'))
        assert active == 0, 'unjoined worker at return/throw'
        if limit <= 0:
            assert lines[i * 2] == 'error' and calls == 0
        elif any(value < 0 for value in values):
            assert lines[i * 2] == 'error' and peak <= limit
        else:
            assert lines[i * 2] == str(sum(value * value for value in values))
            assert calls == len(values) and peak <= limit
            if min(limit, len(values)) >= 2:
                assert peak >= 2, 'sequential implementation rejected'


def transactional(work, tinox):
    import shutil
    name = 'tinox-ai-transaction-' + str(os.getpid()) + '-' + str(time.monotonic_ns())
    subprocess.run(['docker', 'run', '--pull=never', '-d', '--rm', '--tmpfs', '/var/lib/postgresql/data:rw,size=256m', '--name', name, '-P', '-e',
                    'POSTGRES_HOST_AUTH_METHOD=trust', os.environ.get('TINOX_EVAL_POSTGRES_IMAGE', 'postgres:17')],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
    try:
        mapped = subprocess.check_output(['docker', 'port', name, '5432/tcp'], text=True).splitlines()[0]
        port = int(mapped.rsplit(':', 1)[1])
        def sql(query):
            return subprocess.run(['psql', '-XAt', '-v', 'ON_ERROR_STOP=1', '-h', '127.0.0.1',
                                   '-p', str(port), '-U', 'postgres', '-d', 'postgres', '-c', query],
                                  capture_output=True, text=True, timeout=5, env=dict(os.environ, PGCONNECT_TIMEOUT='2'))
        deadline = time.monotonic() + 25
        while sql('SELECT 1').returncode:
            if time.monotonic() > deadline:
                raise RuntimeError('PostgreSQL readiness deadline')
            time.sleep(.1)
        setup = sql("CREATE TABLE accounts (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL, balance BIGINT NOT NULL CHECK (balance >= 0 AND (id != 2 OR balance <= 500))); INSERT INTO accounts VALUES (1,'Sender',1000),(2,'Receiver',200)")
        assert setup.returncode == 0, setup.stderr
        # Consume workspace DB sources without silently testing a registry version.
        repo = Path(__file__).resolve().parents[3]
        shutil.rmtree(work / '.tinox', ignore_errors=True)
        shutil.copytree(repo / 'crates/tinox-core-ext/db/tinox', work / '.tinox/deps/tinox.core/db/checkout/tinox')
        (work / 'tinox.toml').write_text('[package]\ncompiler="2.8.0"\nname="transaction_accept"\nversion="0.0.0"\n[database]\ndriver="postgres"\nurl="postgres://postgres@127.0.0.1:' + str(port) + '/postgres"\npool=2\n')
        calls = [(1, 2, 100), (1, 2, 2000), (1, 2, 300), (1, 99, 2), (1, 1, 2), (1, 2, 0), (1, 2, -2), (1, 2, 50)]
        body = ['const bank = new BankService()', 'const selected: Int64 = envGet("TINOX_EVAL_CASE").toInt()']
        for index, (a, b, amount) in enumerate(calls):
            body.append('if (selected == ' + str(index) + ') { try { bank.transfer(' + str(a) + ', ' + str(b) + ', ' + str(amount) + ')\nprintln("ok") } catch (error: String) { println("error") } }')
        replace_main(work, 'fnc main() -> Int32 {\n' + '\n'.join(body) + '\nreturn 0\n}')
        build(work, tinox)
        for index in range(len(calls)):
            result = subprocess.run([work / 'accepted'], cwd=work, capture_output=True, text=True,
                                    env=dict(os.environ, TINOX_EVAL_CASE=str(index)), timeout=15)
            if index == 2:
                # The current ORM aborts on a database constraint error.
                # Transaction atomicity must still hold after disconnect;
                # no claim of in-process recovery is inferred from this case.
                assert result.returncode != 0, result.stdout + result.stderr
            else:
                assert result.returncode == 0, result.stdout + result.stderr
                assert result.stdout.strip() == ('ok' if index in (0, 7) else 'error')
            expected = ['1|850', '2|350'] if index == 7 else ['1|900', '2|300']
            state = sql('SELECT id,balance FROM accounts ORDER BY id')
            assert state.returncode == 0 and state.stdout.splitlines() == expected, state.stdout

    finally:
        subprocess.run(['docker', 'rm', '-fv', name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)


def broker(work, tinox, compact_publish=False):
    import base64
    import shutil
    import urllib.request
    name = 'tinox-ai-broker-' + str(os.getpid()) + '-' + str(time.monotonic_ns())
    subprocess.run(['docker', 'run', '--pull=never', '-d', '--rm', '--tmpfs', '/var/lib/rabbitmq:rw,size=256m', '--name', name, '-P',
                    '-e', 'RABBITMQ_DEFAULT_USER=eval', '-e', 'RABBITMQ_DEFAULT_PASS=eval',
                    os.environ.get('TINOX_EVAL_RABBITMQ_IMAGE', 'rabbitmq:4-management')], check=True, stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL, timeout=30)
    try:
        def mapped(port):
            text = subprocess.check_output(['docker', 'port', name, str(port) + '/tcp'], text=True).splitlines()[0]
            return int(text.rsplit(':', 1)[1])
        amqp, management = mapped(5672), mapped(15672)
        def api(path, method='GET', body=None):
            data = None if body is None else json.dumps(body).encode()
            req = urllib.request.Request('http://127.0.0.1:' + str(management) + '/api/' + path,
                                         data=data, method=method,
                                         headers={'Authorization': 'Basic ' + base64.b64encode(b'eval:eval').decode(), 'Content-Type': 'application/json'})
            try:
                with urllib.request.urlopen(req, timeout=3) as response:
                    raw = response.read()
                    return json.loads(raw) if raw else None
            except urllib.error.HTTPError as error:
                raise RuntimeError(error.read().decode()) from error
        deadline = time.monotonic() + 45
        while True:
            try:
                api('overview')
                break
            except Exception:
                if time.monotonic() >= deadline:
                    raise RuntimeError('RabbitMQ readiness deadline')
                time.sleep(.2)
        api('queues/%2F/evaluation', 'PUT', {'durable': True, 'auto_delete': False, 'arguments': {}})
        payloads = [b'', bytes(range(256)), b'\x00first\xfflast\x00', b'abc' * 70000]
        for payload in payloads:
            result = api('exchanges/%2F/amq.default/publish', 'POST',
                         {'properties': {'content_type': 'application/octet-stream'}, 'routing_key': 'evaluation',
                          'payload': base64.b64encode(payload).decode(), 'payload_encoding': 'base64'})
            assert result['routed']
        repo = Path(__file__).resolve().parents[3]
        shutil.rmtree(work / '.tinox', ignore_errors=True)
        shutil.copytree(repo / 'crates/tinox-core-ext/amqp091/tinox', work / '.tinox/deps/tinox.core/amqp091/checkout/tinox')
        (work / 'tinox.toml').write_text('[package]\ncompiler="2.8.0"\nname="broker_accept"\nversion="0.0.0"\n')
        main = '''fnc main() -> Int32 {
 const connection = AmqpConnection091::connect("127.0.0.1", PORT, "/", "eval", "eval")
 if (connection.conn <= 0) { throw connection.errorMessage }
 try {
  const ch = AmqpChannel091::open(connection)
  if (ch.chanId <= 0) { throw ch.errorMessage }
  const empty = Processor::drain(ch, "evaluation", 0)
  if (empty.len() != 0) { throw "zero count failed" }
  var rejected: Bool = false
  try { Processor::drain(ch, "evaluation", -1) } catch (error: String) { rejected = true }
  if (!rejected) { throw "negative count accepted" }
  const bodies = Processor::drain(ch, "evaluation", 4)
  if (bodies.len() != 4) { throw "wrong message count" }
  var i: Int64 = 0
  while (i < bodies.len()) {
   bodies[i].writeFile("payload-" + i.toString())
   i = i + 1
  }
 } finally { connection.close() }
 return 0
}'''.replace('PORT', str(amqp))
        if compact_publish:
            publish = '''const replyQueue = ch.declareQueue("replies", true, false, false)
  if (replyQueue == "" || !ch.confirmSelect()) { throw "publish setup failed" }
  for bytes in bodies {
   const tag = ch.publishBytes("", replyQueue, bytes, "application/octet-stream")
   const confirm = ch.waitForConfirm()
   if (tag <= 0 || !confirm.ok || !confirm.ack || confirm.deliveryTag != tag) { throw "compact publish not confirmed" }
  }
'''
            main = main.replace('  var i: Int64 = 0', '  ' + publish + '\n  var i: Int64 = 0')
        replace_main(work, main)
        build(work, tinox)
        subprocess.run([work / 'accepted'], cwd=work, capture_output=True, text=True, check=True, timeout=15)
        assert [(work / ('payload-' + str(i))).read_bytes() for i in range(len(payloads))] == payloads
        # An independent basic.get after the consumer closes must find no
        # unacknowledged/redelivered payload, regardless of cached UI counters.
        remaining = api('queues/%2F/evaluation/get', 'POST', {'count': 10, 'ackmode': 'ack_requeue_true', 'encoding': 'base64', 'truncate': 50000})
        assert remaining == [], 'unacknowledged messages reappeared'
        if compact_publish:
            replies = api('queues/%2F/replies/get', 'POST', {'count': 10, 'ackmode': 'ack_requeue_false', 'encoding': 'base64', 'truncate': 500000})
            assert [base64.b64decode(row['payload']) for row in replies] == payloads
    finally:
        subprocess.run(['docker', 'rm', '-fv', name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)


if __name__ == "__main__":
    task, directory, compiler = sys.argv[1:]
    work = Path(directory)
    if task == "bugfix":
        bugfix(work, compiler)
    elif task == "broker-bytes":
        broker(work, compiler, compact_publish=True)
    elif task == "broker":
        broker(work, compiler)
    elif task == "transactional":
        transactional(work, compiler)
    elif task == "concurrent":
        concurrent(work, compiler)
    elif task in ("validation", "crud"):
        http_check(task, work, compiler)
    else:
        raise ValueError("unknown task")
