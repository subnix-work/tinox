"""Protocol regressions; no paid calls, models, or extra Python packages."""
import copy
import http.server
import threading
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import compare
import runner


class EvaluationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.config = json.loads((runner.HERE / "config.example.json").read_text())
        self.config.update(context_tokens=4096, max_output_tokens=64, token_budget=50000,
                           runs=2, max_turns=3, wall_seconds=20, call_seconds=5)
        self.adapter = self.root / "adapter.py"
        self.adapter.write_text('''import json, sys
request = json.load(sys.stdin)
print(json.dumps({"content": json.dumps({"tool":"submit", "args":{}}),
                  "usage":{"input_tokens":100,"output_tokens":10}}))
''')
        self.config["command"] = [sys.executable, str(self.adapter)]

    def tearDown(self):
        self.temp.cleanup()

    def evaluate(self, process=None):
        if process is None:
            return runner.evaluate(self.config, "bugfix", "/missing/compiler", self.root, 1, 0)
        with patch.object(runner, "process", side_effect=process):
            return self.evaluate()

    def test_unconfigured_execution_never_calls_provider(self):
        config = self.root / "config.json"
        config.write_text(json.dumps(self.config))
        output = self.root / "not-created"
        completed = subprocess.run([sys.executable, str(runner.HERE / "runner.py"),
                                    "--config", str(config), "--out", str(output)], capture_output=True)
        self.assertEqual(completed.returncode, 0)
        self.assertFalse(output.exists())

    def test_model_claim_does_not_pass_acceptance(self):
        self.adapter.write_text('import json; print(json.dumps({"content":"All tests pass!", "usage":{"input_tokens":100,"output_tokens":10}}))')
        result = self.evaluate()
        self.assertFalse(result["correct"])
        self.assertEqual(result["status"], "turn_limit")
        self.assertEqual(result["input_tokens"], 300)
        self.assertTrue(all(e["failed"] for e in result["events"]))

    def test_failing_build_and_retrieval_are_in_next_prompt(self):
        guide = self.root / "guide"
        guide.write_text("held context")
        self.config["context"] = {"guide": str(guide)}
        actions = iter([{"tool": "read", "args": {"path": "docs/guide"}},
                        {"tool": "build"}, {"tool": "submit"}])
        requests = []

        def fake(command, cwd, timeout, payload=None, env=None):
            if payload is not None:
                requests.append(json.loads(payload))
                return 0, json.dumps({"content": json.dumps(next(actions)),
                                      "usage": {"input_tokens": 100, "output_tokens": 20}}), ""
            if command[0] == "/missing/compiler":
                return 1, "", "compiler error"
            return 1, "", "independent rejection"

        result = self.evaluate(fake)
        self.assertEqual(result["input_tokens"], 300)
        self.assertEqual(result["output_tokens"], 60)
        self.assertEqual(result["build_failures"], 1)
        self.assertEqual(result["acceptance_attempts"], 1)
        self.assertIn("held context", json.dumps(requests[-1]))
        self.assertIn("compiler error", json.dumps(requests[-1]))
        self.assertFalse(result["correct"])

    def test_submission_snapshot_and_first_attempt(self):
        def fake(command, cwd, timeout, payload=None, env=None):
            if payload is not None:
                return 0, json.dumps({"content": '{"tool":"submit"}', "usage": {"input_tokens": 100, "output_tokens": 10}}), ""
            snapshot = Path(command[-2])
            self.assertNotEqual(snapshot.parent, cwd)
            self.assertTrue((snapshot / "Main.tnx").exists())
            self.assertFalse((snapshot / "accept.py").exists())
            return 0, "", ""
        result = self.evaluate(fake)
        self.assertTrue(result["correct"])
        self.assertTrue(result["first_attempt_success"])
        self.assertEqual(result["acceptance_attempts"], 1)

    def test_token_limit_prevents_call(self):
        self.config["token_budget"] = 1
        with patch.object(runner, "process") as call:
            result = self.evaluate()
        call.assert_not_called()
        self.assertEqual(result["status"], "token_limit")

    def test_cost_limit_prevents_call(self):
        self.config["input_usd_per_million"] = 1
        with patch.object(runner, "process") as call:
            result = self.evaluate()
        call.assert_not_called()
        self.assertEqual(result["status"], "cost_limit")

    def test_unknown_usage_stops_and_is_not_zero_cost(self):
        self.config["input_usd_per_million"] = 1
        self.adapter.write_text('print("not json")')
        result = runner.evaluate(self.config, "bugfix", "/missing", self.root, 1, 1)
        self.assertFalse(result["accounting_complete"])
        self.assertEqual(result["status"], "provider_error")
        self.assertEqual(len(result["events"]), 1)
        self.assertGreater(result["cost_usd"], 0)

    def test_context_overflow_does_not_evict_history(self):
        self.config["context_tokens"] = 8
        with patch.object(runner, "process") as call:
            result = self.evaluate()
        call.assert_not_called()
        self.assertEqual(result["status"], "context_limit")

    def test_path_escape_is_rejected(self):
        for path in ("../escape", "/etc/passwd"):
            with self.assertRaises(ValueError):
                runner.local_path(self.root, path)
        (self.root / "link").symlink_to("/etc")
        with self.assertRaises(ValueError):
            runner.local_path(self.root, "link/passwd")

    def test_timeout_kills_process(self):
        with self.assertRaises(TimeoutError):
            runner.process([sys.executable, "-c", "import time; time.sleep(5)"], self.root, .05)

    def test_invalid_config_rejected(self):
        for key, value in (("runs", 1), ("max_cost_usd", float("nan")), ("token_budget", True),
                           ("command", []), ("call_seconds", 0), ("billing", "unknown"), ("billing", "metered"), ("api_key", "secret")):
            with self.subTest(key=key):
                config = dict(self.config, **{key: value})
                with self.assertRaises(ValueError):
                    runner.validate(config)

    def test_summary_includes_failed_runs_and_variability(self):
        first = self.evaluate()
        second = dict(first, correct=True, input_tokens=first["input_tokens"] + 100)
        stats = runner.summary([first, second])["bugfix"]
        self.assertEqual(stats["successes"], 1)
        self.assertGreater(stats["input_tokens"]["stdev"], 0)
        self.assertEqual(stats["runs"], 2)

    def test_comparison_rejects_confounded_settings(self):
        first = {"config": self.config, "compiler_sha256": "one", "compiler_version": "1", "started_utc": "a"}
        second = copy.deepcopy(first)
        second["compiler_sha256"] = "two"
        self.assertEqual(compare.comparable(first, "compiler"), compare.comparable(second, "compiler"))
        second["config"]["settings"]["temperature"] = .5
        self.assertNotEqual(compare.comparable(first, "compiler"), compare.comparable(second, "compiler"))


class AdapterTests(unittest.TestCase):
    def test_ollama_adapter_uses_pinned_digest_and_provider_usage(self):
        paths = []
        requests = []

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                paths.append(self.path)
                self.send_response(200)
                self.end_headers()
                self.wfile.write(json.dumps({"models": [{"name": "local:test", "digest": "fixed"}]}).encode())

            def do_POST(self):
                paths.append(self.path)
                requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
                self.send_response(200)
                self.end_headers()
                self.wfile.write(json.dumps({"done": True, "message": {"content": '{"tool":"list"}'},
                                            "prompt_eval_count": 321, "eval_count": 19}).encode())

        server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            call = {"model": "local:test", "model_revision": "fixed", "messages": [],
                    "settings": {"temperature": 0}, "context_tokens": 4096, "max_output_tokens": 128}
            env = dict(os.environ, TINOX_EVAL_OLLAMA_URL=f"http://127.0.0.1:{server.server_port}")
            command = [sys.executable, str(runner.HERE / "ollama_adapter.py")]
            result = subprocess.run(command, input=json.dumps(call), text=True, capture_output=True, env=env)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)["usage"], {"input_tokens": 321, "output_tokens": 19})
            self.assertEqual(paths, ["/api/tags", "/api/chat"])
            self.assertFalse(requests[0]["stream"])
            self.assertEqual(requests[0]["options"]["num_predict"], 128)
            paths.clear()
            call["model_revision"] = "wrong"
            result = subprocess.run(command, input=json.dumps(call), text=True, capture_output=True, env=env)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(paths, ["/api/tags"])
        finally:
            server.shutdown()
            server.server_close()
            thread.join()


class RealCompilerChecks(unittest.TestCase):
    @unittest.skipUnless(os.environ.get("TINOX_EVAL_TEST_COMPILER"), "set TINOX_EVAL_TEST_COMPILER for real acceptance regression")
    def test_bugfix_positive_and_negative_controls(self):
        compiler = str(Path(os.environ["TINOX_EVAL_TEST_COMPILER"]).resolve())
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory) / "work"
            shutil.copytree(runner.HERE / "tasks/bugfix/starter", work)
            command = [sys.executable, str(runner.HERE / "checks/accept.py"), "bugfix", str(work), compiler]
            self.assertNotEqual(subprocess.run(command, capture_output=True).returncode, 0)
            shutil.rmtree(work)
            shutil.copytree(runner.HERE / "tasks/bugfix/starter", work)
            path = work / "Main.tnx"
            path.write_text(path.read_text().replace("return price * quantity - discount", "if (price < 0 || quantity < 0 || discount < 0 || discount > 100) { return -1 }\nreturn price * quantity * (100 - discount) / 100"))
            completed = subprocess.run(command, capture_output=True)
            self.assertEqual(completed.returncode, 0, completed.stderr.decode())


    @unittest.skipUnless(os.environ.get("TINOX_EVAL_TEST_COMPILER"), "set TINOX_EVAL_TEST_COMPILER for real runner regression")
    def test_full_cli_repeats_and_comparison(self):
        compiler = str(Path(os.environ["TINOX_EVAL_TEST_COMPILER"]).resolve())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            adapter = root / "adapter.py"
            source = (runner.HERE / "tasks/bugfix/starter/Main.tnx").read_text().replace(
                "return price * quantity - discount",
                "if (price < 0 || quantity < 0 || discount < 0 || discount > 100) { return -1 }\nreturn price * quantity * (100 - discount) / 100")
            adapter.write_text("import json, sys\nrequest=json.load(sys.stdin)\n" +
                "action=" + repr({"tool": "write", "args": {"path": "Main.tnx", "content": source}}) + "\n" +
                "if len(request['messages']) > 2: action={'tool':'submit'}\n" +
                "print(json.dumps({'content':json.dumps(action),'usage':{'input_tokens':100,'output_tokens':30}}))\n")
            config = json.loads((runner.HERE / "config.example.json").read_text())
            config.update(command=[sys.executable, str(adapter)], runs=2, max_turns=3)
            config_path = root / "config.json"
            config_path.write_text(json.dumps(config))
            output = root / "results"
            result = subprocess.run([sys.executable, str(runner.HERE / "runner.py"), "--config", str(config_path),
                                     "--out", str(output), "--tinox", compiler, "--tasks", "bugfix", "--execute"],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            report = compare.compare(output, output, "compiler")
            self.assertEqual(report["baseline"]["bugfix"]["successes"], 2)
            self.assertEqual(report["baseline"]["bugfix"]["input_tokens"]["mean"], 200)
            (output / "bugfix-002.json").unlink()
            with self.assertRaises(ValueError):
                compare.compare(output, output, "compiler")


if __name__ == "__main__":
    unittest.main()
