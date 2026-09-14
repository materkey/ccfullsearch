# Rust quality gate

The gate runs on PRs and direct pushes to `main`. It compares `scb-check==0.2.0`
measurements of `src` with the last accepted snapshot on the `badges` branch.
Inline Rust tests remain part of this scope; these are whole-source metrics,
not production-only complexity measurements.

A change fails when either condition holds:

- `erosion` **and** `high_cc_mass` both increase.
- `cog_erosion` **and** `high_cog_mass` both increase.

Each pair is evaluated independently with only `1e-9` numerical tolerance.
Deleting simple code can increase a ratio without increasing complex-function
mass, and adding functionality can increase mass without increasing its share;
these cases pass. This is a heuristic for changes that need review, not proof
of code quality: aggregate ratios can still hide individual problems.

SCB `verbosity`, `clone_loc`, and code size remain visible in the report but do
not fail this gate. The existing cargo-crap gate independently rejects new
duplicate pairs, new CRAP hot spots, and CRAP regressions.

Only a successful CI run on the current `main` tip publishes the next baseline.
Failures preserve the accepted baseline and still publish report artifacts.

## Scoped exceptions

For necessary increases, add `.github/scb-exception.json` in the same change,
with a concrete explanation and measured absolute ceilings. There is no global
skip switch. An example (replace the placeholder IDs with full Git object IDs):

```json
{
  "baseline": "<head from the accepted scb-baseline.json>",
  "source_tree": "<git rev-parse HEAD:src>",
  "reason": "Explain why the extra complexity is needed and the alternatives considered; link the issue or review.",
  "limits": {
    "erosion": {"ratio": 0.045, "mass": 450.0}
  }
}
```

Commit source edits before obtaining `source_tree`; adding the exception later
does not change that tree ID. Obtain the accepted revision with:

```sh
git fetch origin badges
git show FETCH_HEAD:scb-baseline.json | jq -r .head
git rev-parse HEAD:src
```

`limits` may name `erosion`, `cog_erosion`, or both. Each entry must bound both
the ratio (0–1, not a percentage) and its corresponding high-complexity mass.
Exceeding either ceiling fails normally. Unnamed regressions are still blocked.
The reason and applied exceptions appear in the summary and JSON artifacts.

The exception expires when `src` or the accepted baseline changes, even if the
file remains in the repository. A stale exception is ignored; malformed entries
fail the job. Remove obsolete exception files in a follow-up change. No exception
is enabled by adding this mechanism, and it does not bypass the CRAP gate.
