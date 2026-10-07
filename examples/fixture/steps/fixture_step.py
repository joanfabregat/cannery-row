"""Shared helpers for the fixture steps (standard library only).

A step reads the container contract under ``CR_ROOT`` (``/cr`` in a
container): ``job.json``, ``inputs/<name>/`` and ``outputs/<name>/``.
"""

import json
import os
import sys
from pathlib import Path
from typing import Any

# Built, not spelled out, so the step code itself never contains the marker.
HELD_OUT_MARKER = "cannery-fixture-" + "held-out-labels"


def root() -> Path:
    return Path(os.environ.get("CR_ROOT", "/cr"))


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, document: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def job() -> dict[str, Any]:
    return read_json(root() / "job.json")


def guard(role: str = "producer") -> None:
    """Candidate code (a producer, an experiment) must never see held-out labels or
    any runner credential."""
    base = root()
    seen = sorted(p.relative_to(base).as_posix() for p in base.rglob("*") if p.is_file())
    print(f"{role} inputs:", ", ".join(seen))
    leaks = [
        name
        for name in seen
        if "qrels" in name or HELD_OUT_MARKER in (base / name).read_text("utf-8", "replace")
    ]
    credentials = [key for key, value in os.environ.items() if value.startswith("cr_")]
    if leaks or credentials:
        print(f"held-out labels {leaks} or credentials {credentials} reached the {role}")
        sys.exit(3)


def guard_producer() -> None:
    guard("producer")


def load_candidate() -> dict[str, Any]:
    candidate = read_json(root() / "inputs" / "candidate" / "candidate.json")
    return candidate if isinstance(candidate, dict) else {}


def rank(scores: dict[str, float], top_k: int) -> list[str]:
    """Best score first; ties by document id so the run is deterministic."""
    return [doc for doc, _ in sorted(scores.items(), key=lambda item: (-item[1], item[0]))][:top_k]


def produce(score: Any) -> None:
    """Rank every document for every query with ``score(query, document)``."""
    guard_producer()
    corpus = read_json(root() / "inputs" / "queries" / "queries.json")
    top_k = int(load_candidate().get("top_k", 10))
    run = {
        query["id"]: rank(
            {doc["id"]: score(query["text"], doc["text"]) for doc in corpus["documents"]}, top_k
        )
        for query in corpus["queries"]
    }
    write_json(root() / "outputs" / "run" / "run.json", {"queries": run})
    print(f"ranked {len(run)} queries, top {top_k}")
