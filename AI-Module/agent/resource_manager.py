"""Resource availability utilities for Phase 5: Local LLM Resource Management.

Checks system RAM and VRAM availability before model loading.  Uses only
standard-library facilities (no psutil) so it works without extra
dependencies.  All public functions return None when measurement is
unavailable rather than raising -- the callers decide whether to treat
missing information as a hard error or a soft warning.

Architecture context:
  TuwaiqOS → Tuwaiq AI → Python Agent → Local Model Runtime
                                              ↑
                                   resource_manager guards here
"""

from __future__ import annotations

import logging
import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

logger = logging.getLogger("tuwaiq_agent.resource_manager")


@dataclass
class SystemResources:
    """Snapshot of relevant system resource availability."""

    available_ram_mb: float | None = None
    total_ram_mb: float | None = None
    # VRAM is optional; None means no GPU or measurement not supported
    available_vram_mb: float | None = None
    total_vram_mb: float | None = None

    def to_dict(self) -> dict[str, Any]:
        return {
            "available_ram_mb": self.available_ram_mb,
            "total_ram_mb": self.total_ram_mb,
            "available_vram_mb": self.available_vram_mb,
            "total_vram_mb": self.total_vram_mb,
        }


def read_system_resources() -> SystemResources:
    """Return a point-in-time snapshot of RAM/VRAM availability.

    Raises nothing -- returns None fields for anything that cannot be read
    so callers can decide how to handle partial information.
    """
    ram = _read_ram()
    vram = _read_vram()
    return SystemResources(
        available_ram_mb=ram.get("available_mb"),
        total_ram_mb=ram.get("total_mb"),
        available_vram_mb=vram.get("available_mb"),
        total_vram_mb=vram.get("total_mb"),
    )


def check_ram_for_profile(min_ram_gb: int) -> tuple[bool, str]:
    """Check whether the system has enough free RAM for the requested profile.

    Returns (ok, reason_string).  ``ok`` is True when available RAM meets
    or exceeds *min_ram_gb*, or when the check cannot be performed (unknown
    beats false-positive rejection).
    """
    resources = read_system_resources()
    if resources.available_ram_mb is None:
        logger.debug("RAM check skipped: could not read available RAM")
        return True, "RAM check skipped (measurement unavailable)"

    required_mb = min_ram_gb * 1024.0
    available = resources.available_ram_mb

    if available >= required_mb:
        return True, (
            f"RAM OK: {available:.0f} MB available >= {required_mb:.0f} MB required"
        )

    return False, (
        f"Insufficient RAM: {available:.0f} MB available, "
        f"{required_mb:.0f} MB required for this model profile"
    )


def check_vram_for_profile(min_vram_gb: int) -> tuple[bool, str]:
    """Check VRAM availability (only meaningful when gpu_layers > 0)."""
    if min_vram_gb <= 0:
        return True, "VRAM check skipped (no GPU layers configured)"

    resources = read_system_resources()
    if resources.available_vram_mb is None:
        logger.debug("VRAM check skipped: could not read available VRAM")
        return True, "VRAM check skipped (measurement unavailable)"

    required_mb = min_vram_gb * 1024.0
    if resources.available_vram_mb >= required_mb:
        return True, (
            f"VRAM OK: {resources.available_vram_mb:.0f} MB available >= "
            f"{required_mb:.0f} MB required"
        )

    return False, (
        f"Insufficient VRAM: {resources.available_vram_mb:.0f} MB available, "
        f"{required_mb:.0f} MB required"
    )


# ---------------------------------------------------------------------------
# Private helpers
# ---------------------------------------------------------------------------


def _read_ram() -> dict[str, float]:
    """Read available and total RAM in MB.  Returns empty dict on failure."""
    if sys.platform == "linux":
        return _read_ram_linux()
    if sys.platform == "darwin":
        return _read_ram_darwin()
    return {}


def _read_ram_linux() -> dict[str, float]:
    try:
        meminfo = Path("/proc/meminfo").read_text(encoding="utf-8")
    except OSError:
        return {}

    values: dict[str, float] = {}
    for line in meminfo.splitlines():
        if line.startswith("MemTotal:"):
            values["total_mb"] = _parse_kb_line(line) / 1024.0
        elif line.startswith("MemAvailable:"):
            values["available_mb"] = _parse_kb_line(line) / 1024.0
        if len(values) == 2:
            break
    return values


def _read_ram_darwin() -> dict[str, float]:
    try:
        out = subprocess.check_output(  # noqa: S603,S607
            ["sysctl", "-n", "hw.memsize"],
            text=True,
            timeout=2,
        ).strip()
        total_bytes = float(out)
        total_mb = total_bytes / (1024.0 * 1024.0)
    except Exception:
        return {}
    # macOS doesn't expose a simple "available" figure without vm_stat parsing;
    # return total only so pre-checks fall through gracefully.
    return {"total_mb": total_mb}


def _parse_kb_line(line: str) -> float:
    """Parse a '/proc/meminfo'-style 'Key: <value> kB' line → kB float."""
    parts = line.split()
    if len(parts) >= 2:
        try:
            return float(parts[1])
        except ValueError:
            pass
    return 0.0


def _read_vram() -> dict[str, float]:
    """Attempt to read VRAM via nvidia-smi.  Silent on failure."""
    try:
        out = subprocess.check_output(  # noqa: S603,S607
            [
                "nvidia-smi",
                "--query-gpu=memory.total,memory.free",
                "--format=csv,noheader,nounits",
            ],
            text=True,
            timeout=3,
        ).strip()
    except Exception:
        return {}

    try:
        parts = out.splitlines()[0].split(",")
        total_mb = float(parts[0].strip())
        free_mb = float(parts[1].strip())
        return {"total_mb": total_mb, "available_mb": free_mb}
    except (IndexError, ValueError):
        return {}
