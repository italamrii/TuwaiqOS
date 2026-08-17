from __future__ import annotations

from pathlib import Path

from inference.predict import InferenceEngine


def test_model_reload_consistency(trained_engine, normal_record: dict) -> None:
    out1 = trained_engine.predict_one(normal_record)

    model_path = Path(trained_engine.model_path)
    reloaded = InferenceEngine(model_path)
    out2 = reloaded.predict_one(normal_record)

    assert out1["anomaly_detected"] == out2["anomaly_detected"]
    assert abs(out1["anomaly_score"] - out2["anomaly_score"]) < 1e-8
