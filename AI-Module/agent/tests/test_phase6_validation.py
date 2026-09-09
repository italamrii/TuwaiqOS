from __future__ import annotations

from pathlib import Path

import pytest

from phase6_validation import (
    MODEL_ORDER,
    ModelBenchmark,
    SecurityCheck,
    build_report,
    render_markdown,
    run_phase6_validation,
    run_static_security_checks,
)


def test_static_security_checks_cover_required_confirmations() -> None:
    checks = {check.name: check for check in run_static_security_checks()}

    assert set(checks) == {
        "no shell access",
        "no unrestricted subprocess execution",
        "no root",
        "no direct OS access",
        "no cloud AI dependency",
        "no bypass around Rust broker",
        "sensitive operations require confirmation",
        "TuwaiqOS remains usable if AI crashes",
    }
    assert checks["no shell access"].passed is True
    assert checks["no direct OS access"].passed is True
    assert checks["no bypass around Rust broker"].passed is True
    assert checks["sensitive operations require confirmation"].passed is True


def test_run_phase6_validation_marks_missing_models_not_ready(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("TUWAIQ_AI_MODEL_ROOT", str(tmp_path / "missing-models"))

    report = run_phase6_validation()

    assert [model.profile_name for model in report.models] == list(MODEL_ORDER)
    assert all(model.status == "NOT AVAILABLE" for model in report.models)
    assert report.readiness_status == "NOT READY"
    assert any("benchmark not run" in issue for issue in report.issues_discovered)


def test_render_markdown_contains_required_deliverable_sections() -> None:
    report = build_report(
        models=[
            ModelBenchmark(
                profile_name="default",
                model_id="qwen3.5-9b-instruct-quantized",
                model_path="/models/default.gguf",
                status="AVAILABLE",
                validation_verdict="PASS",
                tested=True,
                runtime="llama.cpp",
                hardware="device=cpu, threads=6, gpu_layers=0, min_ram_gb=16",
                startup_time_ms=100.0,
                model_loading_time_ms=95.0,
                inference_latency_ms=250.0,
                ram_usage_mb=2048.0,
                vram_usage_mb=None,
                cpu_usage_percent=65.0,
                gpu_usage_percent=None,
                tool_call_success=1.0,
                structured_tool_request_validity="pass",
                response_quality="4/4 functional prompts returned non-empty grounded responses.",
                context_handling="pass: diagnosis → follow-up → close-it → confirmation flow reused prior context",
                security_result="pass: sensitive action required a separate explicit confirmation turn",
                stability="pass",
            )
        ],
        security_checks=[
            SecurityCheck(name="no shell access", passed=True, note="ok"),
            SecurityCheck(name="no unrestricted subprocess execution", passed=True, note="ok"),
            SecurityCheck(name="no root", passed=True, note="ok"),
            SecurityCheck(name="no direct OS access", passed=True, note="ok"),
            SecurityCheck(name="no cloud AI dependency", passed=True, note="ok"),
            SecurityCheck(name="no bypass around Rust broker", passed=True, note="ok"),
            SecurityCheck(name="sensitive operations require confirmation", passed=True, note="ok"),
            SecurityCheck(name="TuwaiqOS remains usable if AI crashes", passed=True, note="ok"),
        ],
    )

    markdown = render_markdown(report)

    for heading in (
        "## 1. Test report",
        "### UNIT TESTS",
        "### REAL LOCAL MODEL TESTS",
        "## 2. Model benchmark report",
        "## 3. Security test report",
        "## 4. End-to-end CLI demo results",
        "## 5. Known limitations",
        "## 6. Recommendation for default model",
        "## 7. List of issues discovered",
        "## Final security confirmations",
    ):
        assert heading in markdown
    assert "READY" in markdown
