"""Fixture producer for the ``character`` track: Jaccard similarity of character n-grams."""

import argparse

from fixture_step import produce


def grams(text: str, n: int) -> set[str]:
    padded = f" {text.lower()} "
    return {padded[i : i + n] for i in range(len(padded) - n + 1)}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--n", type=int, default=3)
    n = parser.parse_args().n

    def score(query: str, document: str) -> float:
        left, right = grams(query, n), grams(document, n)
        return len(left & right) / len(left | right)

    produce(score)


if __name__ == "__main__":
    main()
