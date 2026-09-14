import importlib.util
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "scb_report", Path(__file__).resolve().parents[1] / "scb-report.py",
)
scb = importlib.util.module_from_spec(spec)
spec.loader.exec_module(scb)


def report():
    return {**{key: 0.1 for key in scb.RATIOS},
            **{key: 10 for key in scb.COUNTS}, "syntax_by_language": {"rust": {}}}


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
        self.assertIn("Findings are informational", text)


if __name__ == "__main__":
    unittest.main()
