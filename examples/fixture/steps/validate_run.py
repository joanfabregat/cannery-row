"""Fixture validator for ``ranked-run/v1``: what its JSON Schema does not say.

The runner runs it on each producer ``run`` output, mounted read-only at
``inputs/run/``, once the output matched the interface's schema. Following
the validator contract, it exits 1 (rejected), saying why on stderr, when a
query ranks the same document twice, and 2 when it cannot check at all: an
uncaught Python exception would exit 1, which the runner would take for a
rejection. It never runs candidate code.
"""

import sys

from fixture_step import read_json, root

REJECTED = 1
BROKEN = 2


def main() -> int:
    try:
        run = read_json(root() / "inputs" / "run" / "run.json")["queries"]
        repeated = [
            query for query, ranking in sorted(run.items()) if len(set(ranking)) < len(ranking)
        ]
    except Exception as exc:  # any failure to check is not a verdict
        print(f"cannot check the run: {type(exc).__name__}", file=sys.stderr)
        return BROKEN
    for query in repeated:
        print(f"query {query} ranks a document twice", file=sys.stderr)
    if repeated:
        return REJECTED
    print(f"checked {len(run)} rankings")
    return 0


if __name__ == "__main__":
    sys.exit(main())
