from __future__ import annotations

import json
import math
import pickle
import random
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from training.scripts.ml_utils import DEFAULT_FEATURE_COLUMNS, SERVICE_STATE_MAP


def _mean(values: list[float]) -> float:
    return sum(values) / len(values) if values else 0.0


def _std(values: list[float], mean_value: float) -> float:
    if not values:
        return 1.0
    var = sum((v - mean_value) ** 2 for v in values) / len(values)
    return math.sqrt(var) if var > 0 else 1.0


def _quantile(values: list[float], q: float) -> float:
    if not values:
        return 0.0
    sorted_vals = sorted(values)
    idx = int(round((len(sorted_vals) - 1) * q))
    idx = max(0, min(len(sorted_vals) - 1, idx))
    return sorted_vals[idx]


def _c_factor(n: int) -> float:
    if n <= 1:
        return 0.0
    if n == 2:
        return 1.0
    return 2.0 * (math.log(n - 1) + 0.5772156649) - 2.0 * (n - 1) / n


class _Node:
    __slots__ = ("feature", "split", "left", "right", "size")

    def __init__(self, feature: int | None, split: float | None, left: Any, right: Any, size: int) -> None:
        self.feature = feature
        self.split = split
        self.left = left
        self.right = right
        self.size = size


class SimpleIsolationForest:
    def __init__(
        self,
        *,
        n_estimators: int,
        max_samples: str | int,
        contamination: float,
        random_state: int,
    ) -> None:
        self.n_estimators = int(n_estimators)
        self.max_samples = max_samples
        self.contamination = float(contamination)
        self.random_state = int(random_state)
        self.trees: list[_Node] = []
        self.sample_size = 0
        self.height_limit = 0
        self.score_threshold = 0.5

    def fit(self, X: list[list[float]]) -> "SimpleIsolationForest":
        if not X:
            raise ValueError("X must not be empty")

        rng = random.Random(self.random_state)
        n_rows = len(X)
        if self.max_samples == "auto":
            self.sample_size = min(256, n_rows)
        else:
            self.sample_size = max(2, min(int(self.max_samples), n_rows))

        self.height_limit = math.ceil(math.log2(self.sample_size))
        self.trees = []

        for i in range(self.n_estimators):
            tree_rng = random.Random(rng.randint(0, 10_000_000) + i)
            sample = [X[tree_rng.randrange(0, n_rows)] for _ in range(self.sample_size)]
            self.trees.append(self._build_tree(sample, 0, tree_rng))

        scores = self.score_samples(X)
        self.score_threshold = _quantile(scores, 1.0 - self.contamination)
        return self

    def _build_tree(self, rows: list[list[float]], height: int, rng: random.Random) -> _Node:
        if height >= self.height_limit or len(rows) <= 1:
            return _Node(None, None, None, None, len(rows))

        n_features = len(rows[0])
        valid_features = []
        for j in range(n_features):
            col = [r[j] for r in rows]
            if min(col) < max(col):
                valid_features.append(j)

        if not valid_features:
            return _Node(None, None, None, None, len(rows))

        feature = valid_features[rng.randrange(0, len(valid_features))]
        values = [r[feature] for r in rows]
        lo = min(values)
        hi = max(values)
        split = rng.uniform(lo, hi)

        left_rows = [r for r in rows if r[feature] < split]
        right_rows = [r for r in rows if r[feature] >= split]
        if not left_rows or not right_rows:
            return _Node(None, None, None, None, len(rows))

        left = self._build_tree(left_rows, height + 1, rng)
        right = self._build_tree(right_rows, height + 1, rng)
        return _Node(feature, split, left, right, len(rows))

    def _path_length(self, row: list[float], node: _Node, height: int) -> float:
        if node.feature is None or node.left is None or node.right is None:
            return float(height + _c_factor(node.size))

        if node.split is None:
            return float(height + _c_factor(node.size))
        
        if row[node.feature] < node.split:
            return self._path_length(row, node.left, height + 1)
        return self._path_length(row, node.right, height + 1)

    def score_samples(self, X: list[list[float]]) -> list[float]:
        if not self.trees:
            raise ValueError("Model is not fitted")
        c = _c_factor(self.sample_size)
        if c <= 0:
            c = 1.0

        out: list[float] = []
        for row in X:
            avg_path = _mean([self._path_length(row, t, 0) for t in self.trees])
            score = 2.0 ** (-avg_path / c)
            out.append(float(score))
        return out

    def predict(self, X: list[list[float]]) -> list[int]:
        scores = self.score_samples(X)
        return [1 if s >= self.score_threshold else 0 for s in scores]


class StandardScalerSimple:
    def __init__(self) -> None:
        self.means: list[float] = []
        self.stds: list[float] = []

    def fit(self, X: list[list[float]]) -> "StandardScalerSimple":
        if not X:
            raise ValueError("X must not be empty")
        n_features = len(X[0])
        self.means = []
        self.stds = []
        for j in range(n_features):
            col = [row[j] for row in X]
            m = _mean(col)
            s = _std(col, m)
            self.means.append(m)
            self.stds.append(s if s > 0 else 1.0)
        return self

    def transform(self, X: list[list[float]]) -> list[list[float]]:
        if not self.means or not self.stds:
            raise ValueError("Scaler is not fitted")
        out: list[list[float]] = []
        for row in X:
            out.append([(row[i] - self.means[i]) / self.stds[i] for i in range(len(row))])
        return out


class IsolationForestPipeline:
    def __init__(self, model: SimpleIsolationForest, scaler: StandardScalerSimple) -> None:
        self.model = model
        self.scaler = scaler

    def fit(self, X: list[list[float]]) -> "IsolationForestPipeline":
        self.scaler.fit(X)
        X_scaled = self.scaler.transform(X)
        self.model.fit(X_scaled)
        return self

    def predict(self, X: list[list[float]]) -> list[int]:
        X_scaled = self.scaler.transform(X)
        return self.model.predict(X_scaled)

    def decision_function(self, X: list[list[float]]) -> list[float]:
        X_scaled = self.scaler.transform(X)
        scores = self.model.score_samples(X_scaled)
        return [self.model.score_threshold - s for s in scores]


def train_isolation_forest(
    X: list[list[float]],
    *,
    contamination: float,
    n_estimators: int,
    max_samples: str | int,
    seed: int,
) -> IsolationForestPipeline:
    model = SimpleIsolationForest(
        n_estimators=n_estimators,
        max_samples=max_samples,
        contamination=contamination,
        random_state=seed,
    )
    pipeline = IsolationForestPipeline(model=model, scaler=StandardScalerSimple())
    pipeline.fit(X)
    return pipeline


def _versioned_name(model_name: str, model_version: str) -> str:
    clean_version = model_version.replace("+", "_").replace("/", "_")
    return f"{model_name}_{clean_version}"


def _resolve_model_version(base_version: str, metadata_dir: Path, model_name: str) -> str:
    proposed = base_version
    proposed_name = _versioned_name(model_name, proposed)
    existing = metadata_dir / f"{proposed_name}.json"
    if existing.exists():
        stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        proposed = f"{base_version}+{stamp}"
    return proposed


def save_model_artifacts(
    *,
    pipeline: IsolationForestPipeline,
    feature_columns: list[str],
    config: dict[str, Any],
    evaluation_summary: dict[str, Any],
    root_dir: Path,
) -> dict[str, Path | str]:
    exported_dir = root_dir / "models" / "exported"
    metadata_dir = root_dir / "models" / "metadata"
    exported_dir.mkdir(parents=True, exist_ok=True)
    metadata_dir.mkdir(parents=True, exist_ok=True)

    model_name = str(config.get("model_name", "tuwaiq_ai_system_intelligence"))
    version = _resolve_model_version(
        str(config.get("model_version", "v0.1.0")), metadata_dir, model_name
    )
    training_date = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")

    file_stem = _versioned_name(model_name, version)
    model_path = exported_dir / f"{file_stem}.pkl"
    metadata_path = metadata_dir / f"{file_stem}.json"

    artifact = {
        "pipeline": pipeline,
        "model_name": model_name,
        "model_version": version,
        "algorithm": "IsolationForest",
        "training_date": training_date,
        "feature_columns": feature_columns,
        "service_state_map": SERVICE_STATE_MAP,
        "preprocessing": {
            "feature_columns": feature_columns,
            "service_state_map": SERVICE_STATE_MAP,
            "timestamp_used_as_model_feature": False,
            "notes": "Detection only. No direct diagnosis is inferred by the model.",
        },
    }
    with model_path.open("wb") as f:
        pickle.dump(artifact, f)

    metadata = {
        "model_name": model_name,
        "model_version": version,
        "training_date": training_date,
        "dataset_version": config.get("dataset_version", "synthetic-v1"),
        "feature_list": feature_columns,
        "preprocessing_information": artifact["preprocessing"],
        "algorithm": "IsolationForest",
        "training_configuration": config,
        "evaluation_summary": evaluation_summary,
        "limitations": [
            "Synthetic telemetry only for this prototype.",
            "Anomaly detection is not root-cause diagnosis.",
            "No kernel/runtime integration yet.",
        ],
    }
    metadata_path.write_text(json.dumps(metadata, indent=2), encoding="utf-8")

    return {
        "model_path": model_path,
        "metadata_path": metadata_path,
        "model_version": version,
    }


def load_artifact(model_path: Path) -> dict[str, Any]:
    with model_path.open("rb") as f:
        artifact = pickle.load(f)
    if "pipeline" not in artifact or "feature_columns" not in artifact:
        raise ValueError("Invalid model artifact format")
    if "feature_columns" not in artifact:
        artifact["feature_columns"] = DEFAULT_FEATURE_COLUMNS
    return artifact
