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
GATE_RULES = {"erosion": "high_cc_mass", "cog_erosion": "high_cog_mass"}
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
    return [ratio for ratio, mass in GATE_RULES.items()
            if current[ratio] - baseline[ratio] > ROUNDING_TOLERANCE
            and current[mass] - baseline[mass] > ROUNDING_TOLERANCE]


def scoped_waivers(path, current, baseline_revision, source_tree):
    exception = json.loads(path.read_text())
    if set(exception) != {"baseline", "source_tree", "reason", "limits"}:
        raise ValueError("SCB exception requires baseline, source_tree, reason and limits")
    for key in ("baseline", "source_tree"):
        value = exception[key]
        if not isinstance(value, str) or len(value) != 40 or any(c not in "0123456789abcdef" for c in value):
            raise ValueError(f"SCB exception {key} must be a full Git object ID")
    reason = exception["reason"]
    if not isinstance(reason, str) or not reason.strip():
        raise ValueError("SCB exception requires a nonempty reason")
    limits = exception["limits"]
    if not isinstance(limits, dict) or not limits or not set(limits) <= GATE_RULES.keys():
        raise ValueError("SCB exception limits must name erosion or cog_erosion")
    for ratio, ceiling in limits.items():
        if not isinstance(ceiling, dict) or set(ceiling) != {"ratio", "mass"}:
            raise ValueError(f"SCB exception {ratio} requires ratio and mass ceilings")
        for key, value in ceiling.items():
            if type(value) not in (int, float) or not math.isfinite(value) or value < 0:
                raise ValueError(f"Invalid SCB exception ceiling: {ratio}.{key}")
        if ceiling["ratio"] > 1:
            raise ValueError("SCB exception ratio ceiling must not exceed 1")
    # A source edit or accepted-baseline update expires the exception.
    if exception["baseline"] != baseline_revision or exception["source_tree"] != source_tree:
        return {}
    return {ratio: reason.strip() for ratio, ceiling in limits.items()
            if current[ratio] <= ceiling["ratio"] + ROUNDING_TOLERANCE
            and current[GATE_RULES[ratio]] <= ceiling["mass"] + ROUNDING_TOLERANCE}


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


def summary(current, baseline, head, base, waived=None):
    waived = waived or {}
    blocked = [key for key in regressions(current, baseline) if key not in waived]
    verdict = "Gate: **FAILED** — " + ", ".join(blocked) if blocked else "Gate: **PASSED**"
    lines = [
        "## Rust quality (scb-check)", "",
        f"`{ANALYZER}` · scope: `{SCOPE}` (including inline Rust tests).",
        f"Comparison: `{base}` → `{head}`.",
        verdict,
        "Each erosion metric blocks only when both its ratio and corresponding complex-function mass increase.",
        "SCB verbosity and clone LOC are diagnostic; cargo-crap separately gates new duplicate pairs.", "",
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
    for key, reason in waived.items():
        lines.extend([f"Scoped exception for `{key}`: {reason}", ""])
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
    detected = regressions(current, baseline)
    exception_path = Path(".github/scb-exception.json")
    waived = {}
    if exception_path.exists():
        source_tree = subprocess.check_output(["git", "rev-parse", "HEAD:src"], text=True).strip()
        waived = {key: reason for key, reason in scoped_waivers(
            exception_path, current, base, source_tree,
        ).items() if key in detected}
    failed = [key for key in detected if key not in waived]
    metadata = {"waived_regressions": waived, "gate": "failed" if failed else "passed", "regressions": detected, "analyzer": ANALYZER, "scope": SCOPE, "head": head, "base": base,
                "delta": {key: current[key] - baseline[key] for key in RATIOS + COUNTS + MASSES}}
    (output / "comparison.json").write_text(json.dumps(metadata, indent=2) + "\n")
    body = summary(current, baseline, head, base, waived)
    (output / "summary.md").write_text(body)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as stream:
            stream.write(body)
    print(body)
    if failed:
        for key in failed:
            mass = GATE_RULES[key]
            print(f"::error::SCB regression: {key} {baseline[key]:.12g} -> {current[key]:.12g}; "
                  f"{mass} {baseline[mass]:.12g} -> {current[mass]:.12g}")
        return 1
    snapshot = {"analyzer": ANALYZER, "scope": SCOPE, "head": head, "report": current, "waivers": waived}
    (output / "scb-baseline.json").write_text(json.dumps(snapshot, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
