from __future__ import annotations

import argparse
import json
import statistics
import sys
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from data_collection.collector.schema import validate_records
from data_collection.collector.storage import write_csv, write_jsonl
from data_collection.collector.synthetic_generator import (
    SyntheticGenerationConfig,
    generate_synthetic_records,
)
from training.scripts.ml_utils import (
    compute_detection_metrics,
    metrics_to_dict,
    preprocess_records,
)
from training.scripts.modeling import save_model_artifacts, train_isolation_forest


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Train Isolation Forest anomaly detector.")
    parser.add_argument(
        "--config",
        type=Path,
        default=ROOT / "training" / "configs" / "isolation_forest_baseline.json",
    )
    parser.add_argument(
        "--input",
        type=Path,
        default=ROOT / "data" / "raw" / "telemetry_synthetic_v1.jsonl",
    )
    parser.add_argument(
        "--generate-if-missing",
        action="store_true",
        default=True,
        help="Generate synthetic data when input does not exist.",
    )
    return parser.parse_args()


def load_config(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def load_records(path: Path) -> list[dict]:
    records: list[dict] = []
    with path.open("r", encoding="utf-8") as f:
        for line in f:
            if line.strip():
                records.append(json.loads(line))
    return records


def ensure_input_data(config: dict, input_path: Path, generate_if_missing: bool) -> list[dict]:
    if input_path.exists():
        records = load_records(input_path)
        validate_records(records)
        return records

    if not generate_if_missing:
        raise FileNotFoundError(f"Input telemetry file not found: {input_path}")

    synth_cfg = SyntheticGenerationConfig(
        n_rows=int(config.get("n_rows", 2400)),
        normal_ratio=float(config.get("normal_ratio", 0.85)),
        sampling_interval_seconds=int(config.get("sampling_interval_seconds", 5)),
        seed=int(config.get("seed", 42)),
    )
    records = generate_synthetic_records(synth_cfg)
    validate_records(records)

    input_path.parent.mkdir(parents=True, exist_ok=True)
    write_jsonl(input_path, records)
    write_csv(ROOT / "data" / "processed" / "telemetry_synthetic_v1.csv", records)
    return records


def main() -> None:
    args = parse_args()
    config = load_config(args.config)

    records = ensure_input_data(config, args.input, args.generate_if_missing)

    validate_records(records)

    feature_columns = list(config.get("feature_columns", []))
    X = preprocess_records(records, feature_columns=feature_columns)

    train_on_normal_only = bool(config.get("train_on_normal_only", True))
    if train_on_normal_only:
        X_train = [x for x, r in zip(X, records) if not bool(r["is_synthetic_anomaly"])]
    else:
        X_train = X

    pipeline = train_isolation_forest(
        X_train,
        contamination=float(config.get("contamination", 0.1)),
        n_estimators=int(config.get("n_estimators", 200)),
        max_samples=config.get("max_samples", "auto"),
        seed=int(config.get("seed", 42)),
    )

    decision = pipeline.decision_function(X)
    pred = pipeline.predict(X)
    y_true = [1 if bool(r["is_synthetic_anomaly"]) else 0 for r in records]

    metrics = compute_detection_metrics(y_true, pred)
    eval_summary = {
        "synthetic_evaluation": True,
        "n_samples": int(len(X)),
        "n_train_samples": int(len(X_train)),
        "decision_score_mean": float(statistics.fmean(decision)) if decision else 0.0,
        "decision_score_std": float(statistics.pstdev(decision)) if len(decision) > 1 else 0.0,
        "detection_metrics": metrics_to_dict(metrics),
    }

    saved = save_model_artifacts(
        pipeline=pipeline,
        feature_columns=feature_columns,
        config=config,
        evaluation_summary=eval_summary,
        root_dir=ROOT,
    )

    run_id = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    experiment_path = ROOT / "training" / "experiments" / f"train_{run_id}.json"
    experiment_path.parent.mkdir(parents=True, exist_ok=True)
    experiment_path.write_text(
        json.dumps(
            {
                "run_id": run_id,
                "config": config,
                "evaluation_summary": eval_summary,
                "model_path": str(saved["model_path"]),
                "metadata_path": str(saved["metadata_path"]),
            },
            indent=2,
        ),
        encoding="utf-8",
    )

    print("Training complete")
    print(f"Model version: {saved['model_version']}")
    print(f"Model artifact: {saved['model_path']}")
    print(f"Metadata: {saved['metadata_path']}")
    print(f"Experiment log: {experiment_path}")
    print(json.dumps(eval_summary, indent=2))


if __name__ == "__main__":
    main()
