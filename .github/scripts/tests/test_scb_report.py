import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "scb_report", Path(__file__).resolve().parents[1] / "scb-report.py",
)
scb = importlib.util.module_from_spec(spec)
spec.loader.exec_module(scb)


def report():
    return {**{key: 0.1 for key in scb.RATIOS},
            **{key: 10 for key in scb.COUNTS + scb.MASSES}, "syntax_by_language": {"rust": {}}}


class ScbReportTests(unittest.TestCase):
    def analyze(self, code=0, data=None, stdout=None):
        output = json.dumps(data if data is not None else report()) if stdout is None else stdout
        with patch.object(scb.subprocess, "run", return_value=subprocess.CompletedProcess(
            args=[], returncode=code, stdout=output, stderr="analyzer diagnostics",
        )) as run:
            result = scb.analyze(Path("."))
            self.assertEqual(run.call_args.args[0], [
                "uvx", "--python", "3.12", "scb-check==0.2.0", "check", "src", "--report",
            ])
            return result

    def test_no_findings_when_exit_zero_then_returns_report(self):
        self.assertEqual(self.analyze(), report())

    def test_findings_when_exit_one_then_returns_report(self):
        self.assertEqual(self.analyze(code=1), report())

    def test_analyzer_error_when_exit_two_then_fails(self):
        with self.assertRaisesRegex(RuntimeError, "analyzer diagnostics"):
            self.analyze(code=2)

    def test_crash_when_exit_one_with_traceback_then_fails(self):
        with self.assertRaises(ValueError):
            self.analyze(code=1, stdout="Traceback: analyzer crashed")

    def test_invalid_metric_when_report_received_then_fails(self):
        for value in (None, True, float("nan"), float("inf"), -0.1, 1.1):
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.analyze(data={**report(), "erosion": value})

    def test_empty_or_mixed_language_scope_when_analyzed_then_fails(self):
        for overrides in ({"files_scanned": 0}, {"syntax_by_language": {"rust": {}, "python": {}}}):
            with self.subTest(overrides=overrides), self.assertRaises(ValueError):
                self.analyze(data={**report(), **overrides})

    def test_growth_when_summarized_then_shows_percentage_points_and_loc_delta(self):
        base = report()
        current = {**base, "erosion": 0.15, "total_loc": 13}
        text = scb.summary(current, base, "head", "base")
        self.assertIn("| erosion | 10.00% | 15.00% | +5.00 pp |", text)
        self.assertIn("| total_loc | 10 | 13 | +3 |", text)
        self.assertIn("including inline Rust tests", text)
        self.assertIn("Gate: **FAILED**", text)

    def test_each_quality_metric_when_increased_then_fails(self):
        for key in scb.GATED_METRICS:
            with self.subTest(key=key):
                base = report()
                current = {**base, key: base[key] + 0.001}
                self.assertEqual(scb.regressions(current, base), [key])

    def test_existing_debt_when_unchanged_or_improved_then_passes(self):
        base = report()
        self.assertEqual(scb.regressions(base, base), [])
        current = {**base, **{key: base[key] / 2 for key in scb.GATED_METRICS}}
        self.assertEqual(scb.regressions(current, base), [])

    def test_simple_code_dilutes_erosion_when_complex_mass_grows_then_fails(self):
        base = report()
        current = {**base, "erosion": 0.05, "cog_erosion": 0.05,
                   "high_cc_mass": 11, "high_cog_mass": 11, "total_loc": 100}
        self.assertEqual(scb.regressions(current, base), ["high_cc_mass", "high_cog_mass"])

    def test_loc_growth_when_quality_unchanged_then_passes(self):
        self.assertEqual(scb.regressions({**report(), "total_loc": 100}, report()), [])

    def test_numeric_roundoff_when_compared_then_passes(self):
        self.assertEqual(scb.regressions({**report(), "erosion": 0.1 + 1e-12}, report()), [])

    def test_baseline_fetch_failure_when_loading_then_fails(self):
        with patch.object(scb.subprocess, "run", side_effect=subprocess.CalledProcessError(1, "git")):
            with self.assertRaises(subprocess.CalledProcessError):
                scb.accepted_baseline()

    def test_accepted_baseline_when_loaded_then_uses_accepted_revision(self):
        snapshot = {"analyzer": scb.ANALYZER, "scope": scb.SCOPE,
                    "head": "a" * 40, "report": report()}
        with patch.object(scb.subprocess, "run"), patch.object(
            scb.subprocess, "check_output", side_effect=["scb-baseline.json\n", json.dumps(snapshot)],
        ):
            self.assertEqual(scb.accepted_baseline(), (report(), "a" * 40))

    def test_missing_snapshot_when_first_run_then_uses_seed(self):
        with patch.object(scb.subprocess, "run"), patch.object(
            scb.subprocess, "check_output", return_value="crap-current.json\n",
        ):
            self.assertIsNone(scb.accepted_baseline())

    def test_quality_regression_when_main_runs_then_fails_and_preserves_diagnostics(self):
        self.check_main(failing=True)

    def test_accepted_quality_when_main_runs_then_emits_next_baseline(self):
        self.check_main(failing=False)

    def check_main(self, failing):
        current = {**report(), "erosion": 0.2 if failing else 0.1}
        with tempfile.TemporaryDirectory(prefix="scb-gate-") as directory:
            output = Path(directory)
            with patch("sys.argv", ["scb-report", "--base-ref", "a" * 40, "--output", directory]), \
                 patch.object(scb, "analyze", return_value=current), \
                 patch.object(scb, "accepted_baseline", return_value=(report(), "c" * 40)), \
                 patch.object(scb.subprocess, "check_output", side_effect=["b" * 40, "a" * 40]), \
                 patch.dict(scb.os.environ, {}, clear=True), patch("builtins.print"):
                self.assertEqual(scb.main(), 1 if failing else 0)
            comparison = json.loads((output / "comparison.json").read_text())
            self.assertEqual(comparison["base"], "c" * 40)
            self.assertEqual(comparison["regressions"], ["erosion"] if failing else [])
            self.assertTrue((output / "summary.md").is_file())
            self.assertEqual((output / "scb-baseline.json").exists(), not failing)


if __name__ == "__main__":
    unittest.main()
