"""Fixture experiment step for the ``scripted`` workflow track.

It reads the hypothesis's ``parameters`` from ``job.json`` (``top_k``, checked
by the fixture science revision's ``hypothesis_fields``), writes the candidate
the track's producer will test, and the run document the runner submits:
``run.md``, whose front matter holds the claims and whose body holds the run
notes. When the attempt has a predecessor whose ``candidate`` was uploaded,
it is staged under ``inputs/candidate/`` and noted in the run notes.
"""

import json

from fixture_step import guard, job, read_json, root, write_json


def main() -> None:
    guard("experiment")
    spec = job()
    top_k = int(spec["parameters"].get("top_k", 3))
    previous = root() / "inputs" / "candidate" / "candidate.json"
    resumed = read_json(previous) if previous.is_file() else None
    write_json(root() / "outputs" / "candidate" / "candidate.json", {"top_k": top_k})
    start = (
        f"Resumed from {spec['inputs']['predecessor']['ref']}'s candidate {resumed}."
        if resumed is not None
        else "Started from scratch."
    )
    claims = [
        {
            "metric": "mrr",
            "value": 1.0,
            "authority": "agent_claim",
            "unit": "ratio",
            "direction": "higher",
            "split": "dev",
            "dimensions": {"language": "en"},
        }
    ]
    # JSON is YAML: each front matter value is written as JSON.
    front_matter = {
        "claims": claims,
        "artifact_roles": ["candidate"],
        "provenance": {"source_revision": "fixture-experiment-1"},
    }
    lines = [f"{key}: {json.dumps(value, sort_keys=True)}" for key, value in front_matter.items()]
    notes = (
        f"# Top {top_k}\n\nKept the top {top_k} documents per query, from the hypothesis "
        f"parameters. {start} The relevant document should stay first on every query.\n"
    )
    run = root() / "outputs" / "run" / "run.md"
    run.parent.mkdir(parents=True, exist_ok=True)
    run.write_text("---\n" + "\n".join(lines) + "\n---\n" + notes, encoding="utf-8")
    print(f"wrote a top-{top_k} candidate for {spec['attempt_ref']}")


if __name__ == "__main__":
    main()
