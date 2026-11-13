import json
import unittest

import claude_adapter as adapter

OK = {"is_error": False, "subtype": "success", "num_turns": 1, "result": "{\"tool\":\"list\",\"args\":{}}",
      "usage": {"input_tokens": 10, "cache_creation_input_tokens": 5, "cache_read_input_tokens": 100,
                "output_tokens": 7},
      "modelUsage": {"claude-test-1": {}}}


class ClaudeAdapterTest(unittest.TestCase):
    def test_first_turn_is_sent_verbatim(self):
        system, prompt = adapter.build_prompt([{"role": "system", "content": "S"}, {"role": "user", "content": "U"}])
        self.assertTrue(system.startswith("S") and "OUTPUT FORMAT" in system)
        self.assertEqual(prompt, "U")

    def test_history_becomes_labelled_transcript(self):
        _, prompt = adapter.build_prompt([{"role": "system", "content": "S"}, {"role": "user", "content": "U1"},
                                          {"role": "assistant", "content": "A1"}, {"role": "user", "content": "U2"}])
        self.assertLess(prompt.index("U1"), prompt.index("A1"))
        self.assertLess(prompt.index("A1"), prompt.index("U2"))
        self.assertTrue(prompt.rstrip().endswith("system instructions."))

    def test_usage_counts_cached_input(self):
        result = adapter.parse_result(json.dumps(OK), "claude-test-1")
        self.assertEqual(result["usage"], {"input_tokens": 115, "output_tokens": 7})
        self.assertEqual(result["content"], OK["result"])

    def test_single_fence_is_unwrapped_but_other_text_is_not(self):
        action = '{"tool":"list","args":{}}'
        self.assertEqual(adapter.unwrap_fence("```json\n" + action + "\n```"), action)
        self.assertEqual(adapter.unwrap_fence("```\n" + action + "\n```"), action)
        for raw in ("Sure!\n```json\n" + action + "\n```", "```json\n" + action + "\n```\n```x```", "text " + action):
            self.assertEqual(adapter.unwrap_fence(raw), raw)

    def test_model_drift_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "pinned model"):
            adapter.parse_result(json.dumps(OK), "claude-other")

    def test_errors_and_extra_turns_are_rejected(self):
        for patch in ({"is_error": True}, {"subtype": "error_max_turns"}, {"num_turns": 2}):
            with self.assertRaises(ValueError):
                adapter.parse_result(json.dumps({**OK, **patch}), "claude-test-1")


if __name__ == "__main__":
    unittest.main()
