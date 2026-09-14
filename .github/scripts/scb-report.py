"""Informational Rust quality comparison using one pinned analyzer and scope."""
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


def analyze(root):
    result = subprocess.run(
        ["uvx", "--python", "3.12", ANALYZER, "check", SCOPE, "--report"],
        cwd=root, capture_output=True, text=True,
    )
    # Findings are informational; usage errors/crashes must not turn CI green.
    if result.returncode not in (0, 1):
        raise RuntimeError(f"scb-check failed ({result.returncode}): {result.stderr}")
    report = json.loads(result.stdout)
    for key in RATIOS + COUNTS:
        value = report.get(key)
        if type(value) not in (int, float) or not math.isfinite(value) or value < 0:
            raise ValueError(f"Invalid scb-check metric: {key}")
        if key in RATIOS and value > 1:
            raise ValueError(f"Invalid scb-check ratio: {key}")
    if report["files_scanned"] == 0 or set(report.get("syntax_by_language", {})) != {"rust"}:
        raise ValueError("Expected a nonempty Rust-only src report")
    return report


def summary(current, baseline, head, base):
    lines = [
        "## Rust quality (scb-check)", "",
        f"`{ANALYZER}` · scope: `{SCOPE}` (including inline Rust tests).",
        f"Comparison: `{base}` → `{head}`. Findings are informational.", "",
        "| Metric | Base | Current | Change |",
        "| --- | ---: | ---: | ---: |",
    ]
    for key in RATIOS + COUNTS:
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
    parser.add_argument("--base-ref", required=True)
    parser.add_argument("--merge-base", action="store_true")
    parser.add_argument("--output", default="quality")
    args = parser.parse_args()
    root = Path.cwd()
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    base = subprocess.check_output(
        ["git", "rev-parse", "--verify", args.base_ref + "^{commit}"], text=True,
    ).strip()
    if args.merge_base:
        base = subprocess.check_output(["git", "merge-base", head, base], text=True).strip()
    output = Path(args.output)
    output.mkdir(parents=True, exist_ok=True)
    current = analyze(root)
    (output / "current.json").write_text(json.dumps(current, indent=2) + "\n")
    # Analyze tracked base files without executing any code from that revision.
    archive = subprocess.check_output(["git", "archive", base])
    with tempfile.TemporaryDirectory(prefix="scb-base-") as directory:
        with tarfile.open(fileobj=io.BytesIO(archive)) as tree:
            tree.extractall(directory, filter="data")
        baseline = analyze(directory)
    (output / "baseline.json").write_text(json.dumps(baseline, indent=2) + "\n")
    metadata = {"analyzer": ANALYZER, "scope": SCOPE, "head": head, "base": base,
                "delta": {key: current[key] - baseline[key] for key in RATIOS + COUNTS}}
    (output / "comparison.json").write_text(json.dumps(metadata, indent=2) + "\n")
    body = summary(current, baseline, head, base)
    (output / "summary.md").write_text(body)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as stream:
            stream.write(body)
    print(body)


if __name__ == "__main__":
    main()
