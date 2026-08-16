from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from data_collection.collector.schema import validate_records
from data_collection.collector.synthetic_generator import (
    SyntheticGenerationConfig,
    generate_synthetic_records,
)
from inference.predict import InferenceEngine
from training.scripts.ml_utils import DEFAULT_FEATURE_COLUMNS, preprocess_records
from training.scripts.modeling import save_model_artifacts, train_isolation_forest


@pytest.fixture(scope="session")
def trained_engine(tmp_path_factory: pytest.TempPathFactory) -> InferenceEngine:
    tmp_root = tmp_path_factory.mktemp("ai_dev_model")
    for rel in ["models/exported", "models/metadata"]:
        (tmp_root / rel).mkdir(parents=True, exist_ok=True)

    records = generate_synthetic_records(
        SyntheticGenerationConfig(n_rows=800, normal_ratio=0.85, seed=42, sampling_interval_seconds=5)
    )
    validate_records(records)

    X = preprocess_records(records, feature_columns=DEFAULT_FEATURE_COLUMNS)
    X_train = [x for x, r in zip(X, records) if not bool(r["is_synthetic_anomaly"])]

    pipeline = train_isolation_forest(
        X_train,
        contamination=0.1,
        n_estimators=120,
        max_samples="auto",
        seed=42,
    )

    result = save_model_artifacts(
        pipeline=pipeline,
        feature_columns=DEFAULT_FEATURE_COLUMNS,
        config={
            "model_name": "test_tuwaiq_ai",
            "model_version": "v0.1.0",
            "dataset_version": "synthetic-test-v1",
            "seed": 42,
            "feature_columns": DEFAULT_FEATURE_COLUMNS,
        },
        evaluation_summary={"synthetic_evaluation": True},
        root_dir=tmp_root,
    )

    return InferenceEngine(Path(result["model_path"]))


@pytest.fixture()
def normal_record() -> dict:
    return {
        "timestamp": "2026-01-01T00:00:00Z",
        "cpu_utilization_pct": 32.0,
        "ram_utilization_pct": 45.0,
        "available_ram_mb": 4500.0,
        "process_count": 120,
        "disk_read_kbps": 1300.0,
        "disk_write_kbps": 950.0,
        "network_in_kbps": 900.0,
        "network_out_kbps": 850.0,
        "uptime_seconds": 4000.0,
        "error_event_count": 1,
        "service_state": "healthy",
        "source": "synthetic",
        "scenario_label": "normal",
        "is_synthetic_anomaly": False,
    }
