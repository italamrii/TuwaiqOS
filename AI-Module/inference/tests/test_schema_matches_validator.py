"""Regression test: keeps
`system_interface/schemas/telemetry_input.schema.json` (the public,
documented contract) and `data_collection/collector/schema.py`'s
`validate_record()` (the actual runtime validator) from silently drifting
apart again.

This exists because of a real bug found during Stage 1 telemetry provider
integration: the public schema was missing `source`, `scenario_label`, and
`is_synthetic_anomaly`, which the real validator required all along.

Strategy: build one sample record containing exactly the fields the public
schema lists as required, with a valid dummy value for each, then run it
through the *real* `validate_record()`. If the schema is missing anything
the validator actually requires, `validate_record` raises immediately with
the precise missing field names.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from data_collection.collector.schema import TelemetryValidationError, validate_record

SCHEMA_PATH = Path(__file__).resolve().parents[2] / "system_interface" / "schemas" / "telemetry_input.schema.json"

_DUMMY_BY_TYPE = {
    "string": "dummy",
    "number": 1.0,
    "integer": 1,
    "boolean": False,
}

_ENUM_OVERRIDES = {
    "service_state": "healthy",
    "source": "future_real",
    "timestamp": "2026-01-01T00:00:00+00:00",
}


def _load_schema() -> dict:
    if not SCHEMA_PATH.exists():
        pytest.skip(f"schema not found at {SCHEMA_PATH} -- adjust SCHEMA_PATH if repo layout differs")
    return json.loads(SCHEMA_PATH.read_text())


def _build_record_from_schema(schema: dict) -> dict:
    record = {}
    for field in schema["required"]:
        if field in _ENUM_OVERRIDES:
            record[field] = _ENUM_OVERRIDES[field]
            continue
        prop = schema["properties"].get(field, {})
        json_type = prop.get("type", "string")
        record[field] = _DUMMY_BY_TYPE.get(json_type, "dummy")
    return record


def test_public_schema_required_fields_satisfy_the_real_validator():
    schema = _load_schema()
    record = _build_record_from_schema(schema)
    try:
        validate_record(record)
    except TelemetryValidationError as exc:
        if "Missing required fields" in str(exc):
            pytest.fail(
                f"The public schema at {SCHEMA_PATH} is missing field(s) that "
                f"validate_record() actually requires: {exc}"
            )


def test_public_schema_properties_cover_every_required_field():
    schema = _load_schema()
    properties = set(schema.get("properties", {}).keys())
    for field in schema["required"]:
        assert field in properties, f"'{field}' is required but has no property definition in the schema"