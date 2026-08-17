"""Schema-conformance test for tuwaiq-telemetry-provider.

Runs the real compiled Rust binary, captures its stdout, and validates the
result against ai_development/system_interface/schemas/telemetry_input.schema.json
using the `jsonschema` library -- not a hand-rolled field check, the actual
schema the rest of the ai_development module already depends on.

Run from telemetry-provider/: pytest test_schema_conformance.py
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import jsonschema
import pytest

BINARY = Path(__file__).parent / "target" / "debug" / "tuwaiq-telemetry-provider"
# Path relative to this file's expected location once merged into the
# repo at telemetry-provider/, sitting alongside ai_development/.
SCHEMA_PATH = Path(__file__).parent.parent / "ai_development" / "system_interface" / "schemas" / "telemetry_input.schema.json"


def _load_schema() -> dict:
    if not SCHEMA_PATH.exists():
        pytest.skip(f"schema not found at {SCHEMA_PATH} -- adjust SCHEMA_PATH if repo layout differs")
    return json.loads(SCHEMA_PATH.read_text())


def _run_provider() -> dict:
    if not BINARY.exists():
        pytest.skip(f"binary not built at {BINARY} -- run `cargo build` in telemetry-provider/ first")
    result = subprocess.run([str(BINARY)], capture_output=True, text=True, timeout=15)
    assert result.returncode == 0, f"provider exited nonzero: {result.stderr}"
    return json.loads(result.stdout)


def test_output_conforms_to_telemetry_input_schema():
    schema = _load_schema()
    snapshot = _run_provider()
    jsonschema.validate(instance=snapshot, schema=schema)


def test_output_has_no_additional_properties_beyond_schema():
    # additionalProperties: false in the schema already enforces this via
    # jsonschema.validate above, but this test names the requirement
    # explicitly so a future schema change that silently loosens it is
    # still caught here.
    schema = _load_schema()
    snapshot = _run_provider()
    assert set(snapshot.keys()) == set(schema["required"])


def test_service_state_is_a_valid_enum_value():
    snapshot = _run_provider()
    assert snapshot["service_state"] in {"healthy", "degraded", "critical", "unknown"}


def test_two_consecutive_snapshots_have_increasing_timestamp_and_uptime():
    first = _run_provider()
    second = _run_provider()
    assert second["timestamp"] > first["timestamp"]
    assert second["uptime_seconds"] >= first["uptime_seconds"]


def test_percent_fields_are_within_bounds():
    snapshot = _run_provider()
    assert 0.0 <= snapshot["cpu_utilization_pct"] <= 100.0
    assert 0.0 <= snapshot["ram_utilization_pct"] <= 100.0
