from __future__ import annotations

import argparse
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from data_collection.collector.schema import validate_records
from data_collection.collector.storage import write_csv, write_jsonl
from data_collection.collector.synthetic_generator import (
    SyntheticGenerationConfig,
    generate_synthetic_records,
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Generate synthetic telemetry data.")
    parser.add_argument(
        "--jsonl-output",
        type=Path,
        default=ROOT / "data" / "raw" / "telemetry_synthetic_v1.jsonl",
        help="Output path for JSONL telemetry.",
    )
    parser.add_argument(
        "--csv-output",
        type=Path,
        default=ROOT / "data" / "processed" / "telemetry_synthetic_v1.csv",
        help="Output path for CSV telemetry.",
    )
    parser.add_argument("--rows", type=int, default=2400)
    parser.add_argument("--normal-ratio", type=float, default=0.85)
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--sampling-interval", type=int, default=5)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    config = SyntheticGenerationConfig(
        n_rows=args.rows,
        normal_ratio=args.normal_ratio,
        seed=args.seed,
        sampling_interval_seconds=args.sampling_interval,
    )

    records = generate_synthetic_records(config)
    validate_records(records)

    write_jsonl(args.jsonl_output, records)
    write_csv(args.csv_output, records)

    print(f"Generated synthetic telemetry records: {len(records)}")
    print(f"JSONL output: {args.jsonl_output}")
    print(f"CSV output: {args.csv_output}")
    print("Note: This data is synthetic and not real TuwaiqOS runtime telemetry.")


if __name__ == "__main__":
    main()
