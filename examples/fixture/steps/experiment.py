"""Fixture experiment step for the ``scripted`` workflow track.

It reads the hypothesis's ``parameters`` from ``job.json`` (``top_k``, checked
by the fixture science revision's ``hypothesis_fields``), writes the candidate
the track's producer will test, and the claimed sheet the runner submits.
When the attempt has a predecessor whose ``candidate`` was uploaded, it is
staged under ``inputs/candidate/`` and noted in the report.
"""

from fixture_step import guard, job, read_json, root, write_json


def main() -> None:
    guard("experiment")
    spec = job()
    top_k = int(spec["parameters"].get("top_k", 3))
    previous = root() / "inputs" / "candidate" / "candidate.json"
    resumed = read_json(previous) if previous.is_file() else None
    write_json(root() / "outputs" / "candidate" / "candidate.json", {"top_k": top_k})
    observations = (
        f"Resumed from {spec['inputs']['predecessor']['ref']}'s candidate {resumed}."
        if resumed is not None
        else "Started from scratch."
    )
    write_json(
        root() / "outputs" / "claimed_sheet" / "claimed_sheet.json",
        {
            "report": {
                "what_was_tried": f"Keep the top {top_k} documents per query.",
                "configuration": f"top_k={top_k}, from the hypothesis parameters.",
                "observations": observations,
                "findings": "The relevant document should stay first on every query.",
                "limitations": "Fixture corpus: a handful of queries.",
                "next_question": "Does a smaller cut still hold?",
                "elapsed_seconds": 1,
                "body_markdown": f"# Top {top_k}\n\nRun by the fixture workflow.",
            },
            "measurements": [
                {
                    "metric": "mrr",
                    "value": 1.0,
                    "authority": "agent_claim",
                    "unit": "ratio",
                    "direction": "higher",
                    "split": "dev",
                    "dimensions": {"language": "en"},
                }
            ],
            "artifact_roles": ["candidate"],
            "provenance": {"source_revision": "fixture-experiment-1"},
        },
    )
    print(f"wrote a top-{top_k} candidate for {spec['attempt_ref']}")


if __name__ == "__main__":
    main()
