from __future__ import annotations

import argparse
import json
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from data_collection.collector.schema import validate_record
from training.scripts.ml_utils import (
    indicator_hints,
    input_summary,
    normalize_anomaly_score,
    preprocess_record,
    severity_from_score,
)
from training.scripts.modeling import load_artifact


class InferenceEngine:
    def __init__(self, model_path: Path) -> None:
        artifact = load_artifact(model_path)
        self.model_path = model_path
        self.pipeline = artifact["pipeline"]
        self.model_name = str(artifact.get("model_name", "tuwaiq_ai_system_intelligence"))
        self.model_version = str(artifact.get("model_version", "unknown"))
        self.feature_columns = list(artifact["feature_columns"])

    def predict_one(self, telemetry: dict[str, Any]) -> dict[str, Any]:
        validate_record(telemetry)

        X = [preprocess_record(telemetry, feature_columns=self.feature_columns)]
        decision = float(self.pipeline.decision_function(X)[0])
        pred_raw = int(self.pipeline.predict(X)[0])

        anomaly_score = normalize_anomaly_score(decision)
        anomaly_detected = pred_raw == 1

        return {
            "timestamp": telemetry["timestamp"],
            "model_name": self.model_name,
            "model_version": self.model_version,
            "anomaly_detected": anomaly_detected,
            "anomaly_score": round(anomaly_score, 6),
            "severity": severity_from_score(anomaly_score),
            "input_summary": input_summary(telemetry),
            "indicators": indicator_hints(telemetry),
            "explanation": "Anomaly detection indicates unusual statistical behavior; it is not a root-cause diagnosis.",
            "inference_generated_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        }


def latest_model_path(root: Path) -> Path:
    exported = root / "models" / "exported"
    candidates = sorted(exported.glob("*.pkl"), key=lambda p: p.stat().st_mtime, reverse=True)
    if not candidates:
        raise FileNotFoundError("No exported model artifacts found.")
    return candidates[0]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run anomaly inference for one telemetry record.")
    parser.add_argument(
        "--model",
        type=Path,
        default=None,
        help="Path to model artifact. Uses latest exported artifact if omitted.",
    )
    parser.add_argument(
        "--input-json",
        type=Path,
        required=True,
        help="Path to a JSON file containing one telemetry object.",
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    model_path = args.model or latest_model_path(ROOT)

    record = json.loads(args.input_json.read_text(encoding="utf-8"))
    engine = InferenceEngine(model_path)
    result = engine.predict_one(record)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
