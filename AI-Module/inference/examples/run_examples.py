from __future__ import annotations

import json
import sys
from pathlib import Path
from inference.predict import InferenceEngine, latest_model_path

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

def scenario_inputs() -> dict[str, dict]:
    return {
        "normal": {
            "timestamp": "2026-01-01T00:00:00Z",
            "cpu_utilization_pct": 31.0,
            "ram_utilization_pct": 43.0,
            "available_ram_mb": 4669.0,
            "process_count": 118,
            "disk_read_kbps": 1250.0,
            "disk_write_kbps": 980.0,
            "network_in_kbps": 820.0,
            "network_out_kbps": 790.0,
            "uptime_seconds": 3600.0,
            "error_event_count": 1,
            "service_state": "healthy",
            "source": "synthetic",
            "scenario_label": "normal",
            "is_synthetic_anomaly": False,
        },
        "cpu_anomaly": {
            "timestamp": "2026-01-01T00:00:05Z",
            "cpu_utilization_pct": 98.0,
            "ram_utilization_pct": 62.0,
            "available_ram_mb": 3113.0,
            "process_count": 165,
            "disk_read_kbps": 2200.0,
            "disk_write_kbps": 1700.0,
            "network_in_kbps": 1200.0,
            "network_out_kbps": 1000.0,
            "uptime_seconds": 3605.0,
            "error_event_count": 6,
            "service_state": "degraded",
            "source": "synthetic",
            "scenario_label": "high_cpu",
            "is_synthetic_anomaly": True,
        },
        "memory_anomaly": {
            "timestamp": "2026-01-01T00:00:10Z",
            "cpu_utilization_pct": 57.0,
            "ram_utilization_pct": 97.0,
            "available_ram_mb": 245.0,
            "process_count": 188,
            "disk_read_kbps": 2100.0,
            "disk_write_kbps": 1500.0,
            "network_in_kbps": 1300.0,
            "network_out_kbps": 920.0,
            "uptime_seconds": 3610.0,
            "error_event_count": 7,
            "service_state": "degraded",
            "source": "synthetic",
            "scenario_label": "high_memory",
            "is_synthetic_anomaly": True,
        },
        "mixed_anomaly": {
            "timestamp": "2026-01-01T00:00:15Z",
            "cpu_utilization_pct": 95.0,
            "ram_utilization_pct": 96.0,
            "available_ram_mb": 290.0,
            "process_count": 230,
            "disk_read_kbps": 9800.0,
            "disk_write_kbps": 9300.0,
            "network_in_kbps": 11000.0,
            "network_out_kbps": 9800.0,
            "uptime_seconds": 3615.0,
            "error_event_count": 12,
            "service_state": "critical",
            "source": "synthetic",
            "scenario_label": "mixed_resource_anomaly",
            "is_synthetic_anomaly": True,
        },
    }


def main() -> None:
    model_path = latest_model_path(ROOT)
    engine = InferenceEngine(model_path)

    outputs = []
    for scenario_name, telemetry in scenario_inputs().items():
        prediction = engine.predict_one(telemetry)
        outputs.append({"scenario": scenario_name, "prediction": prediction})

    output_path = ROOT / "inference" / "examples" / "example_outputs.json"
    output_path.write_text(json.dumps(outputs, indent=2), encoding="utf-8")

    print(f"Saved inference examples to: {output_path}")
    print(json.dumps(outputs, indent=2))


if __name__ == "__main__":
    main()
