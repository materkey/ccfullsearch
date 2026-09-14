"""Reject Rust quality regressions against the last accepted scb-check baseline."""
import argparse
import io
import json
import math
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile


ANALYZER = "scb-check==0.2.0"
SCOPE = "src"
RATIOS = ("erosion", "cog_erosion", "verbosity")
COUNTS = ("clone_loc", "total_loc", "total_functions", "high_cc_functions",
          "high_cog_functions", "files_scanned")
MASSES = ("high_cc_mass", "high_cog_mass")
GATED_METRICS = RATIOS + MASSES + ("clone_loc",)
# Numerical roundoff only, not a budget for quality degradation.
ROUNDING_TOLERANCE = 1e-9


def analyze(root):
    result = subprocess.run(
        ["uvx", "--python", "3.12", ANALYZER, "check", SCOPE, "--report"],
        cwd=root, capture_output=True, text=True,
    )
    # Exit 1 means findings; compare their metrics rather than rejecting all debt.
    if result.returncode not in (0, 1):
        raise RuntimeError(f"scb-check failed ({result.returncode}): {result.stderr}")
    return validate_report(json.loads(result.stdout))


def validate_report(report):
    for key in RATIOS + COUNTS + MASSES:
        value = report.get(key)
        if type(value) not in (int, float) or not math.isfinite(value) or value < 0:
            raise ValueError(f"Invalid scb-check metric: {key}")
        if key in RATIOS and value > 1:
            raise ValueError(f"Invalid scb-check ratio: {key}")
    if report["files_scanned"] == 0 or set(report.get("syntax_by_language", {})) != {"rust"}:
        raise ValueError("Expected a nonempty Rust-only src report")
    return report


def regressions(current, baseline):
    return [key for key in GATED_METRICS
            if current[key] - baseline[key] > ROUNDING_TOLERANCE]


def accepted_baseline():
    # A fetch error must fail closed; only the explicit initial seed can be
    # used before the first scb baseline has been published.
    subprocess.run(["git", "fetch", "--no-tags", "--depth", "1", "origin", "badges"], check=True)
    files = subprocess.check_output(["git", "ls-tree", "--name-only", "FETCH_HEAD"], text=True)
    if "scb-baseline.json" not in files.splitlines():
        return None
    snapshot = json.loads(subprocess.check_output(
        ["git", "show", "FETCH_HEAD:scb-baseline.json"], text=True,
    ))
    if snapshot.get("analyzer") != ANALYZER or snapshot.get("scope") != SCOPE:
        raise ValueError("SCB baseline analyzer/scope mismatch; baseline migration required")
    revision = snapshot.get("head", "")
    if len(revision) != 40 or any(c not in "0123456789abcdef" for c in revision):
        raise ValueError("Invalid SCB baseline revision")
    return validate_report(snapshot["report"]), revision


def summary(current, baseline, head, base):
    lines = [
        "## Rust quality (scb-check)", "",
        f"`{ANALYZER}` · scope: `{SCOPE}` (including inline Rust tests).",
        f"Comparison: `{base}` → `{head}`.",
        "Gate: **FAILED** — " + ", ".join(regressions(current, baseline))
        if regressions(current, baseline) else "Gate: **PASSED** — no quality regressions.",
        "Erosion, cognitive erosion, verbosity, complex-function masses and clone LOC must not increase.", "",
        "| Metric | Base | Current | Change |",
        "| --- | ---: | ---: | ---: |",
    ]
    for key in RATIOS + COUNTS + MASSES:
        before, after = baseline[key], current[key]
        if key in RATIOS:
            cells = f"{before:.2%} | {after:.2%} | {(after - before) * 100:+.2f} pp"
        else:
            cells = f"{before:g} | {after:g} | {after - before:+g}"
        lines.append(f"| {key} | {cells} |")
    lines.extend(["", "For Rust, verbosity measures clone lines only. "
                  "SCB complexity and clone metrics differ from cargo-crap; "
                  "SLOC growth alone is not a quality regression.", ""])
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-ref", required=True, help="Known-good seed revision before first baseline publication")
    parser.add_argument("--output", default="quality")
    args = parser.parse_args()
    root = Path.cwd()
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    base = subprocess.check_output(
        ["git", "rev-parse", "--verify", args.base_ref + "^{commit}"], text=True,
    ).strip()
    output = Path(args.output)
    output.mkdir(parents=True, exist_ok=True)
    current = analyze(root)
    (output / "current.json").write_text(json.dumps(current, indent=2) + "\n")
    accepted = accepted_baseline()
    if accepted is not None:
        baseline, base = accepted
    else:
        # The workflow pins a known-green initial seed, never the preceding
        # (possibly failing) push. No base-revision code is executed.
        archive = subprocess.check_output(["git", "archive", base])
        with tempfile.TemporaryDirectory(prefix="scb-base-") as directory:
            with tarfile.open(fileobj=io.BytesIO(archive)) as tree:
                tree.extractall(directory, filter="data")
            baseline = analyze(directory)
    (output / "baseline.json").write_text(json.dumps(baseline, indent=2) + "\n")
    failed = regressions(current, baseline)
    metadata = {"gate": "failed" if failed else "passed", "regressions": failed, "analyzer": ANALYZER, "scope": SCOPE, "head": head, "base": base,
                "delta": {key: current[key] - baseline[key] for key in RATIOS + COUNTS + MASSES}}
    (output / "comparison.json").write_text(json.dumps(metadata, indent=2) + "\n")
    body = summary(current, baseline, head, base)
    (output / "summary.md").write_text(body)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as stream:
            stream.write(body)
    print(body)
    if failed:
        for key in failed:
            print(f"::error::SCB regression: {key} {baseline[key]:.12g} -> {current[key]:.12g}")
        return 1
    snapshot = {"analyzer": ANALYZER, "scope": SCOPE, "head": head, "report": current}
    (output / "scb-baseline.json").write_text(json.dumps(snapshot, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
