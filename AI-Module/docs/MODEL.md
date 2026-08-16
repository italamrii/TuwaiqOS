# Model

## Exported Artifact

The exported model artifact (.pkl) includes:
- Trained pure-Python pipeline (standard scaling + Isolation Forest)
- Model name and version
- Training date
- Feature list
- Preprocessing metadata

## Metadata File

Each model has a metadata JSON containing:
- model_name
- model_version
- training_date
- dataset_version
- feature_list
- preprocessing_information
- algorithm
- training_configuration
- evaluation_summary

## Versioning Convention

- Base version from config, e.g. v0.1.0
- If same version exists, a UTC build suffix is appended, e.g. v0.1.0+20260810T120000Z
- No silent overwriting of model metadata/artifacts

## Expected Input

Schema aligned telemetry object with required fields.
Reference:
- data_collection/schemas/telemetry.schema.json

## Expected Output

Structured result includes:
- anomaly_detected
- anomaly_score
- severity
- model_version
- timestamp
- input_summary
- indicators
- explanation

## Interpretation Guidance

- anomaly_detected means behavior is statistically unusual relative to trained baseline.
- It does not prove exact root cause.

## Known Limitations

- Trained on synthetic data only.
- Not integrated with live TuwaiqOS telemetry.
- Not production hardened.
