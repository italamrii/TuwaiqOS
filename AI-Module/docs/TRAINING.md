# Training

## Baseline Choice

Isolation Forest is used as the first model because:
- Works for anomaly detection without a large labeled dataset.
- Suitable for synthetic bootstrap stage.
- Lightweight and explainable for prototype-level review.

## Pipeline Steps

1. Load telemetry data (JSONL).
2. Validate each record against schema constraints.
3. Preprocess numeric fields and encode service_state to service_state_code.
4. Select curated feature list (timestamp excluded from model input).
5. Train Isolation Forest.
6. Export model artifact.
7. Export model metadata.
8. Run synthetic evaluation metrics.
9. Log experiment outputs.

## Configuration

Default config file:
- training/configs/isolation_forest_baseline.json

Key settings:
- seed
- contamination
- n_estimators
- normal_ratio and n_rows for synthetic generation
- feature_columns

## Reproducibility

Determinism controls:
- Fixed random seed for synthetic generation and model training.
- Explicit training config file.
- Exported metadata includes config and feature list.

## Run

```powershell
python training\scripts\train.py
```

## Outputs

- models/exported/*.pkl
- models/metadata/*.json
- training/experiments/*.json

## Limitations

- Synthetic telemetry only.
- Unsupervised model outputs are probabilistic and environment-dependent.
- Detection does not imply diagnosis.
