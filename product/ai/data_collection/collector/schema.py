from __future__ import annotations

import json
from datetime import datetime
from pathlib import Path
from typing import Any

SCHEMA_PATH = Path(__file__).resolve().parents[1] / "schemas" / "telemetry.schema.json"
SERVICE_STATES = {"healthy", "degraded", "critical", "unknown"}


class TelemetryValidationError(ValueError):
    pass


def load_telemetry_schema(schema_path: Path | None = None) -> dict[str, Any]:
    path = schema_path or SCHEMA_PATH
    with path.open("r", encoding="utf-8") as f:
        return json.load(f)


def _is_iso8601(value: str) -> bool:
    try:
        datetime.fromisoformat(value.replace("Z", "+00:00"))
        return True
    except ValueError:
        return False


def validate_record(record: dict[str, Any]) -> None:
    required = {
        "timestamp",
        "cpu_utilization_pct",
        "ram_utilization_pct",
        "available_ram_mb",
        "process_count",
        "disk_read_kbps",
        "disk_write_kbps",
        "network_in_kbps",
        "network_out_kbps",
        "uptime_seconds",
        "error_event_count",
        "service_state",
        "source",
        "scenario_label",
        "is_synthetic_anomaly",
    }

    missing = required - set(record.keys())
    if missing:
        raise TelemetryValidationError(f"Missing required fields: {sorted(missing)}")

    if not _is_iso8601(str(record["timestamp"])):
        raise TelemetryValidationError("timestamp must be an ISO8601 datetime string")

    bounded_0_100 = ["cpu_utilization_pct", "ram_utilization_pct"]
    non_negative = [
        "available_ram_mb",
        "process_count",
        "disk_read_kbps",
        "disk_write_kbps",
        "network_in_kbps",
        "network_out_kbps",
        "uptime_seconds",
        "error_event_count",
    ]

    for field in bounded_0_100:
        value = float(record[field])
        if value < 0 or value > 100:
            raise TelemetryValidationError(f"{field} must be between 0 and 100")

    for field in non_negative:
        value = float(record[field])
        if value < 0:
            raise TelemetryValidationError(f"{field} must be non-negative")

    if int(record["process_count"]) != record["process_count"]:
        raise TelemetryValidationError("process_count must be an integer")
    if int(record["error_event_count"]) != record["error_event_count"]:
        raise TelemetryValidationError("error_event_count must be an integer")

    if record["service_state"] not in SERVICE_STATES:
        raise TelemetryValidationError(
            f"service_state must be one of: {sorted(SERVICE_STATES)}"
        )

    if record["source"] not in {"synthetic", "future_real"}:
        raise TelemetryValidationError("source must be 'synthetic' or 'future_real'")

    if not isinstance(record["is_synthetic_anomaly"], bool):
        raise TelemetryValidationError("is_synthetic_anomaly must be boolean")


def validate_records(records: list[dict[str, Any]]) -> None:
    for i, record in enumerate(records):
        try:
            validate_record(record)
        except TelemetryValidationError as exc:
            raise TelemetryValidationError(f"Record index {i}: {exc}") from exc
