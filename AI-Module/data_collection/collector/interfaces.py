from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Protocol


@dataclass(frozen=True)
class CollectionConfig:
    output_path: Path
    format: str = "jsonl"
    rows: int = 1000
    sampling_interval_seconds: int = 5
    seed: int = 42


class TelemetryCollector(Protocol):
    def collect(self, config: CollectionConfig) -> list[dict]:
        ...
