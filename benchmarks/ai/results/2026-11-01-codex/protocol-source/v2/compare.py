#!/usr/bin/env python3
"""Compare repeated conditions while rejecting undeclared protocol changes."""
import argparse
import copy
import json
from pathlib import Path


def comparable(protocol, variable):
    result = copy.deepcopy(protocol)
    for key in ("started_utc", "repository_commit", "dirty_tree"):
        result.pop(key, None)
    result["config"].pop("condition", None)
    if variable == "compiler":
        for key in ("compiler_sha256", "compiler_version"):
            result.pop(key, None)
    elif variable == "context":
        result.pop("context_sha256", None)
        result["config"].pop("context", None)
    elif variable == "runner":
        result.pop("runner_sha256", None)
    return result


def compare(baseline, changed, variable):
    protocols = [json.loads((p / "protocol.json").read_text()) for p in (baseline, changed)]
    if comparable(protocols[0], variable) != comparable(protocols[1], variable):
        raise ValueError("conditions differ beyond the declared variable; inspect protocol.json")
    summaries = []
    for directory, protocol in zip((baseline, changed), protocols):
        runs = [json.loads(p.read_text()) for p in sorted(directory.glob("*-*.json"))]
        expected = {(task, index) for task in protocol["tasks"] for index in range(1, protocol["config"]["runs"] + 1)}
        if {(r["task"], r["run"]) for r in runs} != expected:
            raise ValueError("incomplete repeated-run matrix")
        if any(not r["accounting_complete"] for r in runs):
            raise ValueError("unknown token usage; cannot compare efficiency")
        from runner import summary
        summaries.append(summary(runs))
    return {"variable": variable, "baseline": summaries[0], "changed": summaries[1],
            "protocols": protocols, "note": "Failures remain in denominators. No automatic efficiency claim."}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("changed", type=Path)
    parser.add_argument("--variable", required=True, choices=["compiler", "context", "runner"])
    args = parser.parse_args()
    print(json.dumps(compare(args.baseline, args.changed, args.variable), indent=2))
