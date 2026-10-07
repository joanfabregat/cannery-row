# H-001, seed-1: BM25 k1 at 0.9

*Retrospective report, written after the fact from the January 2025 notebook (lines 1-60) and the run's `metrics.json`.*

## What was tried

One run of the lexical retriever with BM25 `k1` lowered to 0.9, on the dev split (200 queries), at code revision `8d1e0f4` with `configs/bm25-k1-0.9.yaml`.

## What was measured

| Slice | MRR | Base camp |
| --- | --- | --- |
| Overall | 0.71 | 0.68 |
| English | 0.74 | |
| French | 0.68 | |

The overall gain over the base camp is 0.03. Both gates of `release-gate` v1 passed: MRR held, and no language regressed.

## Limitations

- One seed only: the notebook records no variance for this run.
- Only the overall value has a recorded control; the per-language baseline was not kept.

## What followed

Promoted as the new lexical default at the weekly review (notebook line 60). H-006 later tuned `b` on top of this `k1`.
