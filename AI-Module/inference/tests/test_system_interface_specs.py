from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def test_system_interface_schemas_exist_and_have_required_fields() -> None:
    telemetry_schema = json.loads(
        (ROOT / "system_interface" / "schemas" / "telemetry_input.schema.json").read_text(encoding="utf-8")
    )
    output_schema = json.loads(
        (ROOT / "system_interface" / "schemas" / "inference_output.schema.json").read_text(encoding="utf-8")
    )

    required_in = set(telemetry_schema["required"])
    required_out = set(output_schema["required"])

    assert "cpu_utilization_pct" in required_in
    assert "ram_utilization_pct" in required_in
    assert "anomaly_detected" in required_out
    assert "anomaly_score" in required_out
