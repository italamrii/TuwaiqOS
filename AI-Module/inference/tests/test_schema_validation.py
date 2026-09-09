from __future__ import annotations

import pytest

from data_collection.collector.schema import TelemetryValidationError, validate_record


def test_validate_record_accepts_valid(normal_record: dict) -> None:
    validate_record(normal_record)


def test_validate_record_rejects_invalid_cpu(normal_record: dict) -> None:
    broken = dict(normal_record)
    broken["cpu_utilization_pct"] = 150.0
    with pytest.raises(TelemetryValidationError):
        validate_record(broken)
