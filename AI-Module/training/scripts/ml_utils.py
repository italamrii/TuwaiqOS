from __future__ import annotations

from dataclasses import dataclass
from typing import Any

SERVICE_STATE_MAP = {
    "healthy": 0,
    "degraded": 1,
    "critical": 2,
    "unknown": 3,
}

DEFAULT_FEATURE_COLUMNS = [
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
    "service_state_code",
]


@dataclass(frozen=True)
class DetectionMetrics:
    true_positives: int
    true_negatives: int
    false_positives: int
    false_negatives: int
    precision: float
    recall: float
    f1_score: float
    false_positive_rate: float
    false_negative_rate: float


def load_records(_: str) -> list[dict[str, Any]]:
    raise NotImplementedError("Use script-level loaders for JSONL/CSV in this prototype")


def preprocess_record(
    record: dict[str, Any], feature_columns: list[str] | None = None
) -> list[float]:
    required = {
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
    }
    missing = required - set(record.keys())
    if missing:
        raise ValueError(f"Missing required preprocessing fields: {sorted(missing)}")

    encoded = dict(record)
    encoded["service_state_code"] = float(
        SERVICE_STATE_MAP.get(str(record.get("service_state", "unknown")), SERVICE_STATE_MAP["unknown"])
    )

    columns = feature_columns or DEFAULT_FEATURE_COLUMNS
    output: list[float] = []
    for col in columns:
        output.append(float(encoded[col]))
    return output


def preprocess_records(
    records: list[dict[str, Any]], feature_columns: list[str] | None = None
) -> list[list[float]]:
    return [preprocess_record(r, feature_columns=feature_columns) for r in records]


def compute_detection_metrics(y_true: list[int], y_pred: list[int]) -> DetectionMetrics:
    tp = 0
    tn = 0
    fp = 0
    fn = 0

    for truth, pred in zip(y_true, y_pred):
        if truth == 1 and pred == 1:
            tp += 1
        elif truth == 0 and pred == 0:
            tn += 1
        elif truth == 0 and pred == 1:
            fp += 1
        elif truth == 1 and pred == 0:
            fn += 1

    precision = tp / (tp + fp) if (tp + fp) > 0 else 0.0
    recall = tp / (tp + fn) if (tp + fn) > 0 else 0.0
    f1 = (2 * precision * recall / (precision + recall)) if (precision + recall) > 0 else 0.0
    fpr = fp / (fp + tn) if (fp + tn) > 0 else 0.0
    fnr = fn / (fn + tp) if (fn + tp) > 0 else 0.0

    return DetectionMetrics(
        true_positives=tp,
        true_negatives=tn,
        false_positives=fp,
        false_negatives=fn,
        precision=precision,
        recall=recall,
        f1_score=f1,
        false_positive_rate=fpr,
        false_negative_rate=fnr,
    )


def metrics_to_dict(metrics: DetectionMetrics) -> dict[str, Any]:
    return {
        "true_positives": metrics.true_positives,
        "true_negatives": metrics.true_negatives,
        "false_positives": metrics.false_positives,
        "false_negatives": metrics.false_negatives,
        "precision": round(metrics.precision, 6),
        "recall": round(metrics.recall, 6),
        "f1_score": round(metrics.f1_score, 6),
        "false_positive_rate": round(metrics.false_positive_rate, 6),
        "false_negative_rate": round(metrics.false_negative_rate, 6),
    }


def normalize_anomaly_score(decision_value: float) -> float:
    return max(0.0, float(-decision_value))


def severity_from_score(score: float) -> str:
    if score >= 0.25:
        return "high"
    if score >= 0.1:
        return "medium"
    if score > 0:
        return "low"
    return "none"


def indicator_hints(record: dict[str, Any]) -> list[str]:
    hints: list[str] = []
    if float(record.get("cpu_utilization_pct", 0)) > 85:
        hints.append("cpu_pressure")
    if float(record.get("ram_utilization_pct", 0)) > 90:
        hints.append("memory_pressure")
    if float(record.get("disk_read_kbps", 0)) > 7000 or float(record.get("disk_write_kbps", 0)) > 7000:
        hints.append("disk_io_pressure")
    if float(record.get("network_in_kbps", 0)) > 7000 or float(record.get("network_out_kbps", 0)) > 7000:
        hints.append("network_pressure")
    if int(record.get("error_event_count", 0)) >= 5:
        hints.append("elevated_error_events")
    return hints


def input_summary(record: dict[str, Any]) -> dict[str, Any]:
    keys = [
        "cpu_utilization_pct",
        "ram_utilization_pct",
        "available_ram_mb",
        "process_count",
        "disk_read_kbps",
        "disk_write_kbps",
        "network_in_kbps",
        "network_out_kbps",
        "error_event_count",
        "service_state",
        "scenario_label",
        "source",
    ]
    return {k: record.get(k) for k in keys}
