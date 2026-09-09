from __future__ import annotations

from training.scripts.ml_utils import DEFAULT_FEATURE_COLUMNS, preprocess_record


def test_preprocess_dataframe_feature_selection(normal_record: dict) -> None:
    vector = preprocess_record(normal_record, feature_columns=DEFAULT_FEATURE_COLUMNS)

    assert len(vector) == len(DEFAULT_FEATURE_COLUMNS)
    assert "timestamp" not in DEFAULT_FEATURE_COLUMNS
    assert DEFAULT_FEATURE_COLUMNS[-1] == "service_state_code"
