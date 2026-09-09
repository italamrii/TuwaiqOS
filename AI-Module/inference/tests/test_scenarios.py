from __future__ import annotations


def test_synthetic_scenarios_relative_scores(trained_engine, normal_record: dict) -> None:
    normal = trained_engine.predict_one(normal_record)

    cpu = dict(normal_record)
    cpu.update(
        {
            "cpu_utilization_pct": 98.0,
            "error_event_count": 7,
            "service_state": "degraded",
            "scenario_label": "high_cpu",
            "is_synthetic_anomaly": True,
        }
    )

    memory = dict(normal_record)
    memory.update(
        {
            "ram_utilization_pct": 97.0,
            "available_ram_mb": 180.0,
            "error_event_count": 8,
            "service_state": "degraded",
            "scenario_label": "high_memory",
            "is_synthetic_anomaly": True,
        }
    )

    disk = dict(normal_record)
    disk.update(
        {
            "disk_read_kbps": 12000.0,
            "disk_write_kbps": 10500.0,
            "error_event_count": 6,
            "service_state": "degraded",
            "scenario_label": "high_disk_io",
            "is_synthetic_anomaly": True,
        }
    )

    network = dict(normal_record)
    network.update(
        {
            "network_in_kbps": 14000.0,
            "network_out_kbps": 13000.0,
            "error_event_count": 5,
            "service_state": "degraded",
            "scenario_label": "high_network",
            "is_synthetic_anomaly": True,
        }
    )

    mixed = dict(normal_record)
    mixed.update(
        {
            "cpu_utilization_pct": 95.0,
            "ram_utilization_pct": 96.0,
            "available_ram_mb": 220.0,
            "disk_read_kbps": 9000.0,
            "disk_write_kbps": 8500.0,
            "network_in_kbps": 9800.0,
            "network_out_kbps": 9300.0,
            "error_event_count": 12,
            "process_count": 220,
            "service_state": "critical",
            "scenario_label": "mixed_resource_anomaly",
            "is_synthetic_anomaly": True,
        }
    )

    outputs = [
        trained_engine.predict_one(cpu),
        trained_engine.predict_one(memory),
        trained_engine.predict_one(disk),
        trained_engine.predict_one(network),
        trained_engine.predict_one(mixed),
    ]

    anomalies_detected = sum(1 for item in outputs if item["anomaly_detected"])
    assert anomalies_detected >= 3

    max_anomaly_score = max(item["anomaly_score"] for item in outputs)
    assert max_anomaly_score >= normal["anomaly_score"]
