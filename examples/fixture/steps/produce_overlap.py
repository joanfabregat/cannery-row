"""Fixture producer for the ``lexical`` track: shared words between query and document."""

from fixture_step import produce


def score(query: str, document: str) -> float:
    return float(len(set(query.lower().split()) & set(document.lower().split())))


if __name__ == "__main__":
    produce(score)
