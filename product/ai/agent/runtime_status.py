"""Factual Product runtime / model status states for Tuwaiq AI.

States (exactly one primary):
  MODEL_NOT_INSTALLED — Ollama missing, unreachable, or qwen3.5:9b absent
  MODEL_LOADING       — runtime reports the model is loading
  MODEL_READY         — model present and endpoint reachable
  READY               — service + broker + model ready for sessions
  ERROR               — broker/API failure that blocks tool use

No model pull is performed here. Boot never downloads weights.
"""

from __future__ import annotations

import json
import logging
import os
import shutil
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from config import AgentConfig

logger = logging.getLogger("tuwaiq_agent.runtime_status")

PRODUCT_MODEL_ID = "qwen3.5:9b"
DEFAULT_BROKER_SOCKET = "/run/tuwaiq/ai-broker.sock"
DEFAULT_API_SOCKET = "/run/tuwaiq/ai.sock"


@dataclass(frozen=True)
class RuntimeStatus:
    state: str
    model_id: str
    provider: str
    ollama_present: bool
    ollama_reachable: bool
    model_installed: bool
    broker_reachable: bool
    detail: str

    def to_dict(self) -> dict[str, Any]:
        return {
            "state": self.state,
            "model_id": self.model_id,
            "provider": self.provider,
            "ollama_present": self.ollama_present,
            "ollama_reachable": self.ollama_reachable,
            "model_installed": self.model_installed,
            "broker_reachable": self.broker_reachable,
            "detail": self.detail,
        }


def _ollama_base(config: AgentConfig) -> str:
    # OpenAI-compatible base is .../v1; tags API is on the Ollama root.
    base = config.openai_base_url
    if base.endswith("/v1"):
        return base[: -len("/v1")]
    return base.rstrip("/")


def _http_json(url: str, timeout: float) -> dict[str, Any] | list[Any] | None:
    try:
        req = urllib.request.Request(url, method="GET")
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except Exception as exc:  # noqa: BLE001 — status probe must never crash
        logger.debug("status probe failed for %s: %s", url, exc)
        return None


def broker_socket_reachable(path: str | None = None) -> bool:
    sock_path = path or os.environ.get("TUWAIQ_AI_BROKER_SOCKET", DEFAULT_BROKER_SOCKET)
    p = Path(sock_path)
    if not p.exists():
        return False
    try:
        import socket

        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(1.0)
        try:
            s.connect(str(p))
            return True
        finally:
            s.close()
    except OSError:
        return False


def probe_model_tags(config: AgentConfig) -> tuple[bool, bool, bool, str]:
    """Return (ollama_reachable, model_installed, model_loading, detail)."""
    root = _ollama_base(config)
    timeout = min(5.0, float(config.request_timeout_seconds))
    data = _http_json(f"{root}/api/tags", timeout=timeout)
    if data is None:
        # Distinguish connection refused from other failures via a cheap open.
        try:
            urllib.request.urlopen(f"{root}/api/tags", timeout=timeout)
        except urllib.error.URLError as exc:
            return False, False, False, f"ollama unreachable: {exc.reason}"
        except Exception as exc:  # noqa: BLE001
            return False, False, False, f"ollama probe failed: {exc}"
        return False, False, False, "ollama tags response unreadable"

    models = []
    if isinstance(data, dict):
        models = data.get("models") or []
    names: list[str] = []
    for entry in models:
        if isinstance(entry, dict):
            name = entry.get("name") or entry.get("model") or ""
            if isinstance(name, str) and name:
                names.append(name)

    target = config.model_id
    # Ollama tags look like "qwen3.5:9b"; accept exact match only for Product.
    installed = target in names

    if not installed:
        return True, False, False, f"model '{target}' not in ollama tags"

    # Optional loading signal: /api/ps lists loaded models.
    ps = _http_json(f"{root}/api/ps", timeout=timeout)
    loading = False
    if isinstance(ps, dict):
        running = ps.get("models") or []
        for entry in running:
            if not isinstance(entry, dict):
                continue
            name = str(entry.get("name") or entry.get("model") or "")
            if name == target or name.startswith(target):
                # size_vram == 0 with digest present can mean still loading on some builds
                if entry.get("size_vram") == 0 and entry.get("size"):
                    loading = True
                break

    return True, True, loading, "model present in ollama"


def collect_status(
    config: AgentConfig | None = None,
    *,
    broker_ok: bool | None = None,
    service_ready: bool = True,
) -> RuntimeStatus:
    cfg = config or AgentConfig.from_environ()
    ollama_present = shutil.which("ollama") is not None
    broker = broker_socket_reachable() if broker_ok is None else broker_ok

    if cfg.provider in {"rule", "rules", "rulebased", "test"}:
        state = "READY" if broker and service_ready else ("ERROR" if not broker else "MODEL_READY")
        return RuntimeStatus(
            state=state,
            model_id=cfg.model_id,
            provider=cfg.provider,
            ollama_present=ollama_present,
            ollama_reachable=False,
            model_installed=True,
            broker_reachable=broker,
            detail="rule provider active (no Ollama required)",
        )

    reachable, installed, loading, detail = probe_model_tags(cfg)

    if not broker:
        return RuntimeStatus(
            state="ERROR",
            model_id=cfg.model_id,
            provider=cfg.provider,
            ollama_present=ollama_present,
            ollama_reachable=reachable,
            model_installed=installed,
            broker_reachable=False,
            detail="broker socket unavailable",
        )

    if not reachable:
        return RuntimeStatus(
            state="MODEL_NOT_INSTALLED",
            model_id=cfg.model_id,
            provider=cfg.provider,
            ollama_present=ollama_present,
            ollama_reachable=False,
            model_installed=False,
            broker_reachable=True,
            detail=detail if ollama_present else "ollama runtime not installed or not running",
        )

    if not installed:
        return RuntimeStatus(
            state="MODEL_NOT_INSTALLED",
            model_id=cfg.model_id,
            provider=cfg.provider,
            ollama_present=ollama_present,
            ollama_reachable=True,
            model_installed=False,
            broker_reachable=True,
            detail=detail,
        )

    if loading:
        return RuntimeStatus(
            state="MODEL_LOADING",
            model_id=cfg.model_id,
            provider=cfg.provider,
            ollama_present=ollama_present,
            ollama_reachable=True,
            model_installed=True,
            broker_reachable=True,
            detail="model is loading in the local runtime",
        )

    if service_ready:
        return RuntimeStatus(
            state="READY",
            model_id=cfg.model_id,
            provider=cfg.provider,
            ollama_present=ollama_present,
            ollama_reachable=True,
            model_installed=True,
            broker_reachable=True,
            detail="service, broker, and model are ready",
        )

    return RuntimeStatus(
        state="MODEL_READY",
        model_id=cfg.model_id,
        provider=cfg.provider,
        ollama_present=ollama_present,
        ollama_reachable=True,
        model_installed=True,
        broker_reachable=True,
        detail="model ready; service not fully accepting sessions",
    )
