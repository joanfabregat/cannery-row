"""Fixture evaluation policy run as a step (role ``evaluator``).

The runner's ``eval`` kind runs it through its launcher with the verified
tester evidence (``inputs/evidence/evidence.json``: ``[{ref, sha256,
record}]``), the tester's output manifest (``inputs/manifest/manifest.json``),
the tester's per-query results (``inputs/per_query_results/``, downloaded
and SHA-256 checked) and ``job.json`` (control, baselines, metric registry).
It writes its verdict to ``outputs/verdict/verdict.json``: ``gates``,
``comparisons``, ``verdict`` and ``reason``. The runner adds the policy
revision, the evidence refs, the producer and the provenance.

Two gates: the overall MRR on dev holds the control the tester reported,
and the per-query results cover every query the evidence counted. Unknown
never passes. ``FIXTURE_POLICY_MODE`` makes it misbehave for tests:
``malformed`` (a pass verdict with a failing gate), ``split`` (a second
file beside the verdict), ``crash`` (exit 3) or
``sleep`` (never finishes in time).
"""

import json
import os
import sys
import time
from typing import Any

from fixture_step import job, read_json, root, write_json


def overall_mrr(records: list[dict[str, Any]]) -> dict[str, Any] | None:
    found = [
        m
        for record in records
        for m in record.get("measurements", [])
        if m.get("metric") == "mrr"
        and m.get("split") == "dev"
        and not m.get("dimensions")
        and m.get("authority") == "tester_verified"
    ]
    return found[0] if len(found) == 1 else None


def judge() -> dict[str, Any]:
    base = root()
    details = job()
    records = [item["record"] for item in read_json(base / "inputs/evidence/evidence.json")]
    manifest = read_json(base / "inputs/manifest/manifest.json")
    registered = {metric["key"] for metric in details["metrics"]}
    gates: list[dict[str, str]] = []
    comparisons: list[dict[str, Any]] = []
    measurement = overall_mrr(records)
    if "mrr" not in registered or measurement is None or "control_value" not in measurement:
        gates.append({"id": "mrr-holds-control", "result": "unknown", "detail": "no overall mrr"})
    else:
        value, control = measurement["value"], measurement["control_value"]
        result = "pass" if value >= control else "fail"
        gates.append(
            {"id": "mrr-holds-control", "result": result, "detail": f"{value} vs {control}"}
        )
        comparisons.append(
            {
                "metric": "mrr",
                "split": "dev",
                "dimensions": {},
                "value": value,
                "source": "tester",
                "reference": {"value": control, "label": "control reported", "kind": "other"},
            }
        )
    rows = 0
    for path in sorted((base / "inputs/per_query_results").rglob("*.jsonl")):
        rows += sum(1 for line in path.read_text(encoding="utf-8").splitlines() if line.strip())
    counted = None if measurement is None else measurement.get("sample_count")
    roles = sorted({obj["role"] for obj in manifest["objects"]})
    covered = "pass" if rows and rows == counted else "fail"
    gates.append(
        {
            "id": "queries-covered",
            "result": covered,
            "detail": f"{rows} per-query rows for {counted} queries; outputs {', '.join(roles)}",
        }
    )
    results = {gate["result"] for gate in gates}
    verdict = "fail" if "fail" in results else "inconclusive" if "unknown" in results else "pass"
    reason = f"{verdict}: " + "; ".join(f"{g['id']} {g['result']}" for g in gates)
    return {"gates": gates, "comparisons": comparisons, "verdict": verdict, "reason": reason}


def main() -> int:
    mode = os.environ.get("FIXTURE_POLICY_MODE", "judge")
    if mode == "crash":
        print("the fixture policy crashes on purpose", file=sys.stderr)
        return 3
    if mode == "sleep":
        time.sleep(600)
    verdict = judge()
    if mode == "malformed":
        verdict["verdict"] = "pass"
        verdict["gates"].append({"id": "always-fails", "result": "fail"})
    write_json(root() / "outputs/verdict/verdict.json", verdict)
    if mode == "split":
        write_json(root() / "outputs/verdict/extra.json", verdict)
    print(json.dumps({"verdict": verdict["verdict"]}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
