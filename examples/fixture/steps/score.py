"""Fixture scorer shared by every track: mean reciprocal rank per language.

It reads the producer's ranked run, the held-out relevance labels and the
front matter of the frozen run document (``claimed.json``), and writes its
evidence (provenance, verified measurements, discrepancies, observations)
plus per-query results. The runner composes the verification report from
the evidence and the policy's verdict. It never runs candidate code.
``FIXTURE_EXTENSIONS``, a JSON object in its environment, becomes the
evidence's ``extensions``.
"""

import json
import os
from typing import Any

from fixture_step import job, read_json, root, write_json

METRIC = {"metric": "mrr", "unit": "ratio", "direction": "higher", "split": "dev"}

# The control's published MRR per slice ("all" for the overall slice), by the
# immutable revision of the control the job pins (none when the unit
# names no control). The scorer resolves the control by its identity rather
# than re-running it; an unknown revision leaves ``control_value`` out. It is
# informational: the stock policy configuration (../policy.json)
# holds the same values and makes a slice unknown if a reported value here
# disagrees.
CONTROL_MRR = {"fixture-r1": {"en": 0.75, "fr": 0.5, "all": 0.625}}


def reciprocal_rank(ranking: list[str], relevant: set[str]) -> float:
    for position, doc in enumerate(ranking, start=1):
        if doc in relevant:
            return 1.0 / position
    return 0.0


def discrepancies(
    claimed: list[dict[str, Any]], verified: list[dict[str, Any]]
) -> list[dict[str, Any]]:
    """Every claimed value that differs from the verified value of the same slice."""
    found: list[dict[str, Any]] = []
    by_slice = {
        (m["metric"], m["split"], tuple(sorted(m.get("dimensions", {}).items()))): m
        for m in verified
    }
    for claim in claimed:
        key = (claim["metric"], claim["split"], tuple(sorted(claim.get("dimensions", {}).items())))
        match = by_slice.get(key)
        if match is None or "value" not in claim or "value" not in match:
            continue
        if abs(claim["value"] - match["value"]) > 1e-9:
            found.append(
                {
                    "metric": claim["metric"],
                    "split": claim["split"],
                    "dimensions": claim.get("dimensions", {}),
                    "claimed_value": claim["value"],
                    "verified_value": match["value"],
                    "description": "The claimed value differs from the measured value.",
                }
            )
    return found


def main() -> None:
    base = root()
    details = job()
    run = read_json(base / "inputs" / "run" / "run.json")["queries"]
    qrels = read_json(base / "inputs" / "qrels" / "qrels.json")["queries"]
    # The front matter of the run document the agent submitted.
    claimed = read_json(base / "inputs" / "claimed_sheet" / "claimed.json")

    rows = []
    for query_id, labels in sorted(qrels.items()):
        rows.append(
            {
                "query": query_id,
                "language": labels["language"],
                "reciprocal_rank": reciprocal_rank(run.get(query_id, []), set(labels["relevant"])),
            }
        )
    measurements: list[dict[str, Any]] = []
    for language in ("en", "fr"):
        values = [row["reciprocal_rank"] for row in rows if row["language"] == language]
        measurements.append(
            {
                **METRIC,
                "value": sum(values) / len(values),
                "authority": "tester_verified",
                "dimensions": {"language": language},
                "sample_count": len(values),
            }
        )
    everything = [row["reciprocal_rank"] for row in rows]
    measurements.append(
        {
            **METRIC,
            "value": sum(everything) / len(everything),
            "authority": "tester_verified",
            "sample_count": len(everything),
        }
    )
    pinned = details.get("control")
    control = CONTROL_MRR.get(pinned["revision"], {}) if pinned else {}
    for measurement in measurements:
        slice_name = measurement.get("dimensions", {}).get("language", "all")
        if slice_name in control:
            measurement["control_value"] = control[slice_name]
    claims = [m for m in claimed.get("claims", []) if m.get("authority") == "agent_claim"]
    # By the input's name: the registered id it reads may differ.
    qrels_revision = next(
        d["revision"] for d in details["inputs"]["datasets"] if d["name"] == "qrels"
    )
    evidence = {
        "provenance": {
            "source_revision": claimed["provenance"]["source_revision"],
            "dataset_revision": qrels_revision,
            "science_revision": details["science_revision"],
            "seed": 0,
        },
        "observations": f"Scored {len(rows)} queries from the {details['producer']['name']} run.",
        "measurements": measurements,
        "discrepancies": discrepancies(claims, measurements),
        "artifact_roles": ["per_query_results"],
    }
    if pinned:
        evidence["provenance"]["control_revision"] = pinned["revision"]
    if "FIXTURE_EXTENSIONS" in os.environ:
        evidence["extensions"] = json.loads(os.environ["FIXTURE_EXTENSIONS"])
    write_json(base / "outputs" / "evidence" / "evidence.json", evidence)
    lines = "".join(json.dumps(row, sort_keys=True) + "\n" for row in rows)
    (base / "outputs" / "per_query_results" / "results.jsonl").write_text(lines, encoding="utf-8")
    print(f"scored {len(rows)} queries")


if __name__ == "__main__":
    main()
