from __future__ import annotations


def test_inference_output_format(trained_engine, normal_record: dict) -> None:
    out = trained_engine.predict_one(normal_record)

    required = {
        "timestamp",
        "model_name",
        "model_version",
        "anomaly_detected",
        "anomaly_score",
        "severity",
        "input_summary",
        "indicators",
        "explanation",
        "inference_generated_at",
    }
    assert required.issubset(out.keys())
    assert isinstance(out["anomaly_detected"], bool)
    assert out["severity"] in {"none", "low", "medium", "high"}
