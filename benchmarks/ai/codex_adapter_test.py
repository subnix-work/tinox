"""Accounting and subscription isolation regressions; no model calls."""
import json
import contextlib
import io
import subprocess
import unittest
from unittest.mock import patch

import codex_adapter
import runner


class CodexAdapterTests(unittest.TestCase):
    def request(self):
        return {"model": "test-model", "model_revision": "test-model@codex-cli test;backend-snapshot-unavailable",
                "settings": {"cli_version": "codex-cli test", "reasoning_effort": "low"},
                "messages": [{"role": "user", "content": "test"}]}

    def events(self):
        return [{"type": "thread.started", "thread_id": "test"},
                {"type": "turn.started"},
                {"type": "item.completed", "item": {"type": "agent_message", "text": '{"tool":"list"}'}},
                {"type": "turn.completed", "usage": {"input_tokens": 100, "cached_input_tokens": 80,
                                                       "output_tokens": 20, "reasoning_output_tokens": 10}}]

    def parse(self, events):
        return codex_adapter.parse_events("\n".join(json.dumps(event) for event in events))

    def test_cache_and_reasoning_are_not_double_counted(self):
        result = self.parse(self.events())
        self.assertEqual(result["usage"]["input_tokens"], 100)
        self.assertEqual(result["usage"]["output_tokens"], 20)
        self.assertEqual(result["provider_metadata"]["backend_snapshot"], "not exposed by CLI")

    def test_native_tools_errors_and_multiple_turns_are_rejected(self):
        for extra in ({"type": "item.completed", "item": {"type": "command_execution"}},
                      {"type": "error", "message": "Reconnecting"},
                      {"type": "turn.failed", "error": {"message": "Usage limit"}},
                      self.events()[-1]):
            with self.subTest(extra=extra), self.assertRaises(ValueError):
                self.parse(self.events() + [extra])

    def test_missing_usage_is_rejected(self):
        events = self.events()
        events[-1]["usage"].pop("input_tokens")
        with self.assertRaises(ValueError):
            self.parse(events)

    def test_commentary_is_retained_and_counted_but_only_final_action_is_executed(self):
        events = self.events()
        events.insert(2, {"type": "item.completed", "item": {"type": "agent_message", "text": "Thinking"}})
        raw = "\n".join(json.dumps(event) for event in events)
        result = codex_adapter.parse_events(raw, '{"tool":"list"}\n')
        self.assertEqual(json.loads(result["content"]), {"tool": "list"})
        self.assertEqual(result["usage"]["output_tokens"], 20)
        self.assertEqual(result["provider_metadata"]["events"], events)
        with self.assertRaises(ValueError):
            codex_adapter.parse_events(raw, '{"tool":"submit"}')

    def test_api_and_parent_session_environment_is_removed(self):
        with patch.dict("os.environ", {"PATH": "/bin", "HOME": "/home/test", "OPENAI_API_KEY": "secret",
                                       "CODEX_API_KEY": "secret", "OPENAI_BASE_URL": "other",
                                       "CODEX_THREAD_ID": "parent"}, clear=True):
            self.assertEqual(codex_adapter.environment(), {"PATH": "/bin", "HOME": "/home/test"})

    def test_subscription_cannot_claim_metered_spending(self):
        config = json.loads((runner.HERE / "config.example.json").read_text())
        config.update(provider="codex-chatgpt", billing="subscription")
        runner.validate(config)
        for changes in ({"provider": "other"}, {"max_cost_usd": 1}, {"input_usd_per_million": 1}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                runner.validate(dict(config, **changes))

    def test_api_login_stops_before_generation(self):
        responses = [subprocess.CompletedProcess([], 0, "codex-cli test\n", ""),
                     subprocess.CompletedProcess([], 0, "", "Logged in using an API key")]
        with patch("sys.stdin", io.StringIO(json.dumps(self.request()))), patch(
                "subprocess.run", side_effect=responses) as run, self.assertRaises(ValueError):
            codex_adapter.main()
        self.assertEqual(run.call_count, 2)

    def test_generation_forces_subscription_and_retains_failed_events(self):
        failed = json.dumps({"type": "turn.failed", "error": {"message": "Usage limit reached"}})
        responses = [subprocess.CompletedProcess([], 0, "codex-cli test\n", ""),
                     subprocess.CompletedProcess([], 0, "", "Logged in using ChatGPT"),
                     subprocess.CompletedProcess([], 1, failed, "private stderr")]
        output = io.StringIO()
        with patch("sys.stdin", io.StringIO(json.dumps(self.request()))), contextlib.redirect_stdout(output), patch(
                "subprocess.run", side_effect=responses) as run:
            codex_adapter.main()
        command = run.call_args.args[0]
        self.assertIn('forced_login_method="chatgpt"', command)
        self.assertIn("--ignore-user-config", command)
        self.assertIn("--ephemeral", command)
        result = json.loads(output.getvalue())
        self.assertNotIn("usage", result)
        self.assertEqual(result["provider_metadata"]["events"], [failed])
        self.assertNotIn("private stderr", output.getvalue())


if __name__ == "__main__":
    unittest.main()
