from __future__ import annotations

import argparse
import json
import statistics
import sys
import time
import tracemalloc
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from data_collection.collector.schema import validate_records
from training.scripts.ml_utils import (
    compute_detection_metrics,
    metrics_to_dict,
    preprocess_records,
)
from training.scripts.modeling import load_artifact


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Evaluate exported anomaly model.")
    parser.add_argument(
        "--model-path",
        type=Path,
        default=None,
        help="Path to exported model. Uses latest artifact if omitted.",
    )
    parser.add_argument(
        "--input",
        type=Path,
        default=ROOT / "data" / "raw" / "telemetry_synthetic_v1.jsonl",
    )
    return parser.parse_args()


def latest_model_path() -> Path:
    exported = ROOT / "models" / "exported"
    candidates = sorted(exported.glob("*.pkl"), key=lambda p: p.stat().st_mtime, reverse=True)
    if not candidates:
        raise FileNotFoundError("No exported models found. Train first.")
    return candidates[0]


def read_jsonl(path: Path) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    with path.open("r", encoding="utf-8") as f:
        for line in f:
            if line.strip():
                records.append(json.loads(line))
    return records


def main() -> None:
    args = parse_args()
    model_path = args.model_path or latest_model_path()

    artifact = load_artifact(model_path)
    pipeline = artifact["pipeline"]
    feature_columns = artifact["feature_columns"]

    records = read_jsonl(args.input)
    validate_records(records)
    X = preprocess_records(records, feature_columns=feature_columns)
    y_true = [1 if bool(r["is_synthetic_anomaly"]) else 0 for r in records]

    tracemalloc.start()
    t0 = time.perf_counter()
    pred_raw = pipeline.predict(X)
    decision = pipeline.decision_function(X)
    elapsed = time.perf_counter() - t0
    _, peak_mem = tracemalloc.get_traced_memory()
    tracemalloc.stop()

    y_pred = [int(v) for v in pred_raw]
    metrics = metrics_to_dict(compute_detection_metrics(y_true, y_pred))

    latency_ms = (elapsed / max(len(X), 1)) * 1000.0
    model_size_bytes = model_path.stat().st_size

    benchmark = {
        "timestamp": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "model_name": artifact.get("model_name"),
        "model_version": artifact.get("model_version"),
        "input_file": str(args.input),
        "sample_count": int(len(X)),
        "synthetic_evaluation": True,
        "metrics": metrics,
        "decision_score_mean": float(statistics.fmean(decision)) if decision else 0.0,
        "decision_score_std": float(statistics.pstdev(decision)) if len(decision) > 1 else 0.0,
        "inference_latency_ms_per_sample": latency_ms,
        "peak_prediction_memory_bytes": int(peak_mem),
        "model_size_bytes": int(model_size_bytes),
    }

    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    benchmark_path = ROOT / "evaluation" / "benchmarks" / f"benchmark_{stamp}.json"
    report_path = ROOT / "evaluation" / "reports" / f"evaluation_{stamp}.md"

    benchmark_path.write_text(json.dumps(benchmark, indent=2), encoding="utf-8")

    report = [
        "# Tuwaiq AI System Intelligence Evaluation Report",
        "",
        f"- Timestamp: {benchmark['timestamp']}",
        f"- Model: {benchmark['model_name']} {benchmark['model_version']}",
        f"- Dataset source: synthetic telemetry ({args.input})",
        "- Note: results are from synthetic scenarios, not real TuwaiqOS runtime telemetry.",
        "",
        "## Detection Metrics",
        f"- Precision: {metrics['precision']:.4f}",
        f"- Recall: {metrics['recall']:.4f}",
        f"- F1 score: {metrics['f1_score']:.4f}",
        f"- False positive rate: {metrics['false_positive_rate']:.4f}",
        f"- False negative rate: {metrics['false_negative_rate']:.4f}",
        f"- TP/TN/FP/FN: {metrics['true_positives']}/{metrics['true_negatives']}/{metrics['false_positives']}/{metrics['false_negatives']}",
        "",
        "## Runtime Characteristics",
        f"- Inference latency per sample (ms): {latency_ms:.6f}",
        f"- Peak prediction memory (bytes): {int(peak_mem)}",
        f"- Model size (bytes): {int(model_size_bytes)}",
        "",
        "## Detection vs Diagnosis",
        "- The model detects statistical anomalies only.",
        "- It does not prove root cause or exact failing subsystem.",
    ]

    report_path.write_text("\n".join(report), encoding="utf-8")

    print(f"Evaluation report: {report_path}")
    print(f"Benchmark JSON: {benchmark_path}")
    print(json.dumps(benchmark, indent=2))


if __name__ == "__main__":
    main()
