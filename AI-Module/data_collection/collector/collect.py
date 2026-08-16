from __future__ import annotations

import argparse
from pathlib import Path

from data_collection.collector.interfaces import CollectionConfig, TelemetryCollector
from data_collection.collector.schema import validate_records
from data_collection.collector.storage import write_csv, write_jsonl
from data_collection.collector.synthetic_generator import (
    SyntheticGenerationConfig,
    generate_synthetic_records,
)


class SyntheticTelemetryCollector(TelemetryCollector):
    def collect(self, config: CollectionConfig) -> list[dict]:
        generator_config = SyntheticGenerationConfig(
            n_rows=config.rows,
            sampling_interval_seconds=config.sampling_interval_seconds,
            seed=config.seed,
        )
        records = generate_synthetic_records(generator_config)
        validate_records(records)

        fmt = config.format.lower()
        if fmt == "jsonl":
            write_jsonl(config.output_path, records)
        elif fmt == "csv":
            write_csv(config.output_path, records)
        else:
            raise ValueError("format must be 'jsonl' or 'csv'")

        return records


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Standalone telemetry collector prototype.")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--format", type=str, default="jsonl", choices=["jsonl", "csv"])
    parser.add_argument("--rows", type=int, default=1200)
    parser.add_argument("--sampling-interval", type=int, default=5)
    parser.add_argument("--seed", type=int, default=42)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    config = CollectionConfig(
        output_path=args.output,
        format=args.format,
        rows=args.rows,
        sampling_interval_seconds=args.sampling_interval,
        seed=args.seed,
    )
    collector = SyntheticTelemetryCollector()
    records = collector.collect(config)

    print(f"Collected {len(records)} synthetic telemetry records to {args.output}")
    print("Collector mode: synthetic prototype only (future real telemetry integration required).")


if __name__ == "__main__":
    main()
