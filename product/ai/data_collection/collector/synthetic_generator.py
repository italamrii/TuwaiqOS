from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
import random
from typing import Any


@dataclass(frozen=True)
class SyntheticGenerationConfig:
    n_rows: int = 2000
    normal_ratio: float = 0.85
    sampling_interval_seconds: int = 5
    seed: int = 42
    total_ram_mb: int = 8192


def _clamp(value: float, low: float, high: float) -> float:
    return float(max(low, min(high, value)))


def _base_record(ts: datetime, uptime_seconds: float) -> dict[str, Any]:
    return {
        "timestamp": ts.isoformat().replace("+00:00", "Z"),
        "cpu_utilization_pct": 0.0,
        "ram_utilization_pct": 0.0,
        "available_ram_mb": 0.0,
        "process_count": 0,
        "disk_read_kbps": 0.0,
        "disk_write_kbps": 0.0,
        "network_in_kbps": 0.0,
        "network_out_kbps": 0.0,
        "uptime_seconds": uptime_seconds,
        "error_event_count": 0,
        "service_state": "healthy",
        "source": "synthetic",
        "scenario_label": "normal",
        "is_synthetic_anomaly": False,
    }


def _normal_sample(base: dict[str, Any], rng: random.Random, total_ram_mb: int) -> dict[str, Any]:
    cpu = _clamp(rng.gauss(35, 8), 5, 75)
    ram = _clamp(rng.gauss(45, 10), 15, 80)
    available = _clamp(total_ram_mb * (1.0 - ram / 100.0), 256, total_ram_mb)

    sample = dict(base)
    sample.update(
        {
            "cpu_utilization_pct": cpu,
            "ram_utilization_pct": ram,
            "available_ram_mb": available,
            "process_count": int(_clamp(rng.gauss(120, 20), 50, 240)),
            "disk_read_kbps": _clamp(rng.gauss(1200, 500), 50, 5000),
            "disk_write_kbps": _clamp(rng.gauss(900, 450), 30, 4000),
            "network_in_kbps": _clamp(rng.gauss(800, 300), 20, 4500),
            "network_out_kbps": _clamp(rng.gauss(750, 280), 20, 4000),
            "error_event_count": int(_clamp(rng.gauss(1, 1), 0, 4)),
            "service_state": "healthy",
            "scenario_label": "normal",
            "is_synthetic_anomaly": False,
        }
    )
    return sample


def _cpu_anomaly(base: dict[str, Any], rng: random.Random, total_ram_mb: int) -> dict[str, Any]:
    sample = _normal_sample(base, rng, total_ram_mb)
    sample.update(
        {
            "cpu_utilization_pct": _clamp(rng.gauss(95, 3), 85, 100),
            "error_event_count": int(_clamp(rng.gauss(4, 2), 1, 12)),
            "service_state": "degraded",
            "scenario_label": "high_cpu",
            "is_synthetic_anomaly": True,
        }
    )
    return sample


def _memory_anomaly(base: dict[str, Any], rng: random.Random, total_ram_mb: int) -> dict[str, Any]:
    ram = _clamp(rng.gauss(96, 2), 88, 100)
    sample = _normal_sample(base, rng, total_ram_mb)
    sample.update(
        {
            "ram_utilization_pct": ram,
            "available_ram_mb": _clamp(total_ram_mb * (1.0 - ram / 100.0), 0, 700),
            "error_event_count": int(_clamp(rng.gauss(5, 2), 1, 12)),
            "service_state": "degraded",
            "scenario_label": "high_memory",
            "is_synthetic_anomaly": True,
        }
    )
    return sample


def _disk_anomaly(base: dict[str, Any], rng: random.Random, total_ram_mb: int) -> dict[str, Any]:
    sample = _normal_sample(base, rng, total_ram_mb)
    sample.update(
        {
            "disk_read_kbps": _clamp(rng.gauss(12000, 2000), 7000, 25000),
            "disk_write_kbps": _clamp(rng.gauss(10500, 1800), 6000, 22000),
            "error_event_count": int(_clamp(rng.gauss(4, 2), 1, 10)),
            "service_state": "degraded",
            "scenario_label": "high_disk_io",
            "is_synthetic_anomaly": True,
        }
    )
    return sample


def _network_anomaly(base: dict[str, Any], rng: random.Random, total_ram_mb: int) -> dict[str, Any]:
    sample = _normal_sample(base, rng, total_ram_mb)
    sample.update(
        {
            "network_in_kbps": _clamp(rng.gauss(14000, 2500), 8000, 30000),
            "network_out_kbps": _clamp(rng.gauss(13000, 2200), 7000, 28000),
            "error_event_count": int(_clamp(rng.gauss(3, 2), 1, 9)),
            "service_state": "degraded",
            "scenario_label": "high_network",
            "is_synthetic_anomaly": True,
        }
    )
    return sample


def _mixed_anomaly(base: dict[str, Any], rng: random.Random, total_ram_mb: int) -> dict[str, Any]:
    ram = _clamp(rng.gauss(95, 2), 88, 100)
    sample = _normal_sample(base, rng, total_ram_mb)
    sample.update(
        {
            "cpu_utilization_pct": _clamp(rng.gauss(93, 4), 80, 100),
            "ram_utilization_pct": ram,
            "available_ram_mb": _clamp(total_ram_mb * (1.0 - ram / 100.0), 0, 900),
            "disk_read_kbps": _clamp(rng.gauss(9000, 1800), 5000, 22000),
            "disk_write_kbps": _clamp(rng.gauss(8800, 1600), 4500, 21000),
            "network_in_kbps": _clamp(rng.gauss(9000, 2000), 4000, 25000),
            "network_out_kbps": _clamp(rng.gauss(8500, 1900), 3500, 23000),
            "error_event_count": int(_clamp(rng.gauss(8, 3), 2, 20)),
            "service_state": "critical",
            "scenario_label": "mixed_resource_anomaly",
            "is_synthetic_anomaly": True,
        }
    )
    return sample


def generate_synthetic_records(config: SyntheticGenerationConfig) -> list[dict[str, Any]]:
    if config.n_rows <= 0:
        raise ValueError("n_rows must be > 0")
    if config.sampling_interval_seconds <= 0:
        raise ValueError("sampling_interval_seconds must be > 0")
    if not (0 < config.normal_ratio < 1):
        raise ValueError("normal_ratio must be between 0 and 1")

    rng = random.Random(config.seed)
    start = datetime(2026, 1, 1, 0, 0, 0, tzinfo=timezone.utc)

    anomaly_generators = [
        _cpu_anomaly,
        _memory_anomaly,
        _disk_anomaly,
        _network_anomaly,
        _mixed_anomaly,
    ]

    n_normal = int(round(config.n_rows * config.normal_ratio))
    n_anomaly = config.n_rows - n_normal

    records: list[dict[str, Any]] = []
    for i in range(config.n_rows):
        ts = start + timedelta(seconds=i * config.sampling_interval_seconds)
        uptime_seconds = float(i * config.sampling_interval_seconds)
        base = _base_record(ts, uptime_seconds)

        if i < n_normal:
            record = _normal_sample(base, rng, config.total_ram_mb)
        else:
            gen = anomaly_generators[(i - n_normal) % len(anomaly_generators)]
            record = gen(base, rng, config.total_ram_mb)

        records.append(record)

    return records
