"""Environment-driven configuration for the Tuwaiq AI agent.

No developer-specific absolute paths are hardcoded. Broker binary may be
overridden; otherwise it is resolved relative to this package tree.
"""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path


def _env_int(name: str, default: int) -> int:
    raw = os.environ.get(name)
    if raw is None or raw.strip() == "":
        return default
    return int(raw)


def _env_float(name: str, default: float) -> float:
    raw = os.environ.get(name)
    if raw is None or raw.strip() == "":
        return default
    return float(raw)


@dataclass(frozen=True)
class AgentConfig:
    """Runtime knobs for provider, loop budget, and evidence freshness."""

    provider: str = "local"  # local | rule
    # OpenAI-compatible local endpoint (vLLM / SGLang / Ollama / etc.)
    openai_base_url: str = "http://127.0.0.1:11434/v1"
    openai_api_key: str = "local"
    # Primary target is Qwen/Qwen3.5-9B; default here is the official smaller
    # development verification identifier when the host cannot host 9B.
    model_id: str = "Qwen/Qwen3.5-4B"
    request_timeout_seconds: float = 60.0
    max_context_chars: int = 24_000
    max_iterations: int = 4
    evidence_ttl_seconds: float = 120.0
    max_session_turns: int = 32
    max_evidence_records: int = 24
    broker_path: str | None = None

    @classmethod
    def from_environ(cls) -> "AgentConfig":
        return cls(
            provider=os.environ.get("TUWAIQ_AI_PROVIDER", "local").strip().lower(),
            openai_base_url=os.environ.get(
                "TUWAIQ_AI_OPENAI_BASE_URL", "http://127.0.0.1:11434/v1"
            ).rstrip("/"),
            openai_api_key=os.environ.get("TUWAIQ_AI_OPENAI_API_KEY", "local"),
            model_id=os.environ.get("TUWAIQ_AI_MODEL", "Qwen/Qwen3.5-4B"),
            request_timeout_seconds=_env_float("TUWAIQ_AI_TIMEOUT_SECONDS", 60.0),
            max_context_chars=_env_int("TUWAIQ_AI_MAX_CONTEXT_CHARS", 24_000),
            max_iterations=_env_int("TUWAIQ_AI_MAX_ITERATIONS", 4),
            evidence_ttl_seconds=_env_float("TUWAIQ_AI_EVIDENCE_TTL_SECONDS", 120.0),
            max_session_turns=_env_int("TUWAIQ_AI_MAX_SESSION_TURNS", 32),
            max_evidence_records=_env_int("TUWAIQ_AI_MAX_EVIDENCE_RECORDS", 24),
            broker_path=os.environ.get("TUWAIQ_AI_BROKER_PATH") or None,
        )

    def resolve_broker_path(self) -> Path:
        if self.broker_path:
            return Path(self.broker_path)
        base = (
            Path(__file__).resolve().parent.parent
            / "broker"
            / "target"
            / "debug"
            / "tuwaiq-agent-broker"
        )
        if base.exists():
            return base
        exe = base.with_suffix(".exe")
        if exe.exists():
            return exe
        return base
