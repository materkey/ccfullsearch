import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parents[1]
PAIR = {
    "first_file": "./src/a.rs", "first_function": "first",
    "first_start_line": 10, "second_file": "./src/b.rs",
    "second_function": "second", "second_start_line": 20, "score": 0.9,
}


class CrapGateTests(unittest.TestCase):
    def run_gate(self, entries=(), duplicates=(), baseline_pairs=(), baseline=True):
        with tempfile.TemporaryDirectory(prefix="crap-gate-") as directory:
            root = Path(directory)
            (root / "baseline").mkdir()
            if baseline:
                (root / "baseline/crap-current.json").write_text(json.dumps({
                    "version": "0.5.0", "entries": [], "duplicates": baseline_pairs,
                }))
            (root / ".cargo-crap.toml").write_text("threshold = 15.0\nepsilon = 1.0\n")
            report = {"entries": entries}
            if duplicates is not None:
                report["duplicates"] = duplicates
            (root / "fixture.json").write_text(json.dumps(report))
            # Exercise the actual shell gate with deterministic analyzer output.
            cargo = root / "cargo"
            cargo.write_text('#!/bin/sh\ncp fixture.json delta.json\n')
            cargo.chmod(0o755)
            return subprocess.run(
                ["bash", str(SCRIPTS / "crap-regression-gate.sh")], cwd=root,
                env={**os.environ, "PATH": str(root) + os.pathsep + os.environ["PATH"]},
                capture_output=True, text=True,
            )

    def test_accepted_baseline_when_scores_unchanged_then_passes(self):
        result = self.run_gate(entries=[{"status": "unchanged", "crap": 20}])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_regression_above_threshold_when_checked_then_fails(self):
        result = self.run_gate(entries=[{"status": "regressed", "crap": 16}])
        self.assertEqual(result.returncode, 1)
        self.assertIn("1 regression(s)", result.stdout)

    def test_new_hotspot_when_checked_then_fails(self):
        result = self.run_gate(entries=[{"status": "new", "crap": 16}])
        self.assertEqual(result.returncode, 1)
        self.assertIn("1 new function(s)", result.stdout)

    def test_score_at_threshold_when_regressed_then_passes(self):
        self.assertEqual(self.run_gate(entries=[{"status": "regressed", "crap": 15}]).returncode, 0)

    def test_new_duplicate_when_scores_pass_then_fails(self):
        result = self.run_gate(duplicates=[PAIR])
        self.assertEqual(result.returncode, 1)
        self.assertIn("1 new duplicate pair(s)", result.stdout)
        self.assertIn("first (./src/a.rs:10)", result.stdout)

    def test_known_pair_when_lines_score_and_endpoint_order_change_then_passes(self):
        reversed_pair = {
            "first_file": PAIR["second_file"], "first_function": PAIR["second_function"],
            "second_file": PAIR["first_file"], "second_function": PAIR["first_function"],
            "first_start_line": 500, "second_start_line": 600, "score": 1.0,
        }
        result = self.run_gate(duplicates=[reversed_pair], baseline_pairs=[PAIR])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_removed_pair_when_checked_then_passes(self):
        self.assertEqual(self.run_gate(baseline_pairs=[PAIR]).returncode, 0)

    def test_replaced_pair_when_total_count_unchanged_then_fails(self):
        new_pair = {**PAIR, "second_function": "third"}
        result = self.run_gate(duplicates=[new_pair], baseline_pairs=[PAIR])
        self.assertEqual(result.returncode, 1)
        self.assertIn("1 new duplicate pair(s)", result.stdout)

    def test_fetch_failure_when_baseline_unavailable_then_fails(self):
        with tempfile.TemporaryDirectory(prefix="crap-fetch-") as directory:
            root = Path(directory)
            git = root / "git"
            git.write_text("#!/bin/sh\nexit 1\n")
            git.chmod(0o755)
            result = subprocess.run(
                ["bash", str(SCRIPTS / "crap-fetch-baseline.sh")], cwd=root,
                env={**os.environ, "PATH": str(root) + os.pathsep + os.environ["PATH"]},
                capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 1)
            self.assertIn("Cannot fetch CRAP baseline", result.stdout)

    def test_missing_baseline_when_checked_then_fails(self):
        result = self.run_gate(baseline=False)
        self.assertEqual(result.returncode, 1)
        self.assertIn("No usable CRAP baseline", result.stdout)

    def test_missing_duplicate_output_when_checked_then_fails(self):
        self.assertNotEqual(self.run_gate(duplicates=None).returncode, 0)

    def test_old_baseline_without_duplicate_analysis_when_checked_then_fails(self):
        self.assertNotEqual(self.run_gate(baseline_pairs=None).returncode, 0)


if __name__ == "__main__":
    unittest.main()
