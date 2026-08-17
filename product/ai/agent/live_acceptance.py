"""Live Arabic acceptance harness for Grounded Local Agent V1 + real Qwen.

Writes measurements and conversation evidence under AI-Module/evidence/
(gitignored). Uses LocalModelProvider only — never RuleBasedProvider.
"""

from __future__ import annotations

import json
import logging
import os
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from agent import Agent  # noqa: E402
from broker_client import BrokerClient  # noqa: E402
from config import AgentConfig  # noqa: E402
from model_provider import LocalModelProvider, RuleBasedProvider, build_provider  # noqa: E402

EVIDENCE_DIR = ROOT.parent / "evidence"
EVIDENCE_DIR.mkdir(parents=True, exist_ok=True)


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


def _nvidia() -> dict:
    import subprocess

    try:
        out = subprocess.check_output(
            [
                "nvidia-smi",
                "--query-gpu=name,memory.total,memory.used,memory.free,utilization.gpu",
                "--format=csv,noheader,nounits",
            ],
            text=True,
            timeout=10,
        ).strip()
        parts = [p.strip() for p in out.split(",")]
        return {
            "name": parts[0],
            "memory_total_mib": float(parts[1]),
            "memory_used_mib": float(parts[2]),
            "memory_free_mib": float(parts[3]),
            "utilization_gpu_pct": float(parts[4]),
        }
    except Exception as exc:  # noqa: BLE001
        return {"error": str(exc)}


def _ram() -> dict:
    import ctypes

    class MEMORYSTATUSEX(ctypes.Structure):
        _fields_ = [
            ("dwLength", ctypes.c_ulong),
            ("dwMemoryLoad", ctypes.c_ulong),
            ("ullTotalPhys", ctypes.c_ulonglong),
            ("ullAvailPhys", ctypes.c_ulonglong),
            ("ullTotalPageFile", ctypes.c_ulonglong),
            ("ullAvailPageFile", ctypes.c_ulonglong),
            ("ullTotalVirtual", ctypes.c_ulonglong),
            ("ullAvailVirtual", ctypes.c_ulonglong),
            ("ullAvailExtendedVirtual", ctypes.c_ulonglong),
        ]

    stat = MEMORYSTATUSEX()
    stat.dwLength = ctypes.sizeof(MEMORYSTATUSEX)
    ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(stat))
    return {
        "total_bytes": stat.ullTotalPhys,
        "available_bytes": stat.ullAvailPhys,
        "used_bytes": stat.ullTotalPhys - stat.ullAvailPhys,
        "load_percent": stat.dwMemoryLoad,
    }


def _post_chat(model: str, base: str, messages: list, tools=None, timeout=120.0) -> dict:
    payload = {"model": model, "messages": messages, "temperature": 0.1, "stream": False}
    if tools is not None:
        payload["tools"] = tools
        payload["tool_choice"] = "auto"
    req = urllib.request.Request(
        f"{base.rstrip('/')}/chat/completions",
        data=json.dumps(payload).encode("utf-8"),
        method="POST",
        headers={"Content-Type": "application/json", "Authorization": "Bearer local"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))


def main() -> int:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(name)s %(levelname)s %(message)s")
    report: dict = {"started_at": _now(), "events": []}

    model_id = os.environ.get("TUWAIQ_AI_MODEL", "qwen3.5:9b")
    base_url = os.environ.get("TUWAIQ_AI_OPENAI_BASE_URL", "http://127.0.0.1:11434/v1")
    broker_path = os.environ.get(
        "TUWAIQ_AI_BROKER_PATH",
        str(ROOT.parent / "evidence" / "run-broker-docker.cmd"),
    )

    # Prove provider selection.
    os.environ["TUWAIQ_AI_PROVIDER"] = "local"
    os.environ["TUWAIQ_AI_MODEL"] = model_id
    os.environ["TUWAIQ_AI_OPENAI_BASE_URL"] = base_url
    os.environ["TUWAIQ_AI_BROKER_PATH"] = broker_path
    os.environ["TUWAIQ_AI_TIMEOUT_SECONDS"] = os.environ.get("TUWAIQ_AI_TIMEOUT_SECONDS", "180")
    os.environ["TUWAIQ_AI_MAX_ITERATIONS"] = os.environ.get("TUWAIQ_AI_MAX_ITERATIONS", "4")

    cfg = AgentConfig.from_environ()
    provider = build_provider(cfg)
    assert isinstance(provider, LocalModelProvider), type(provider)
    assert not isinstance(provider, RuleBasedProvider)
    report["provider_class"] = type(provider).__name__
    report["config"] = {
        "provider": cfg.provider,
        "model_id": cfg.model_id,
        "openai_base_url": cfg.openai_base_url,
        "broker_path": str(cfg.resolve_broker_path()),
        "max_iterations": cfg.max_iterations,
    }

    report["pre_load_ram"] = _ram()
    report["pre_load_gpu"] = _nvidia()

    # Measure cold load / first token via a trivial completion.
    t0 = time.perf_counter()
    warm = _post_chat(
        model_id,
        base_url,
        [{"role": "user", "content": "قل مرحبا بكلمة واحدة فقط."}],
        timeout=300.0,
    )
    load_s = time.perf_counter() - t0
    report["model_load_or_first_response_seconds"] = round(load_s, 3)
    report["warmup_response"] = ((warm.get("choices") or [{}])[0].get("message") or {}).get("content")
    report["post_load_gpu"] = _nvidia()
    report["post_load_ram"] = _ram()

    broker = BrokerClient(cfg.resolve_broker_path())
    agent = Agent(model=provider, broker=broker, config=cfg)

    turns = [
        "ليش جهازي بطيء؟",
        "وش أكثر برنامج مستهلك؟",
        "سكره",
    ]
    conversation = []
    for msg in turns:
        before_audit = len(agent.audit_events)
        t1 = time.perf_counter()
        reply = agent.handle(msg)
        elapsed = time.perf_counter() - t1
        new_events = agent.audit_events[before_audit:]
        turn = {
            "user": msg,
            "assistant": reply,
            "elapsed_seconds": round(elapsed, 3),
            "audit_events": new_events,
            "evidence_tools": [e["tool"] for e in agent.memory.evidence_summary()],
            "pending_confirmation": agent.memory.pending_confirmation,
        }
        conversation.append(turn)
        print(f"\n=== USER ===\n{msg}\n=== ASSISTANT ({elapsed:.2f}s) ===\n{reply}\n")

    report["conversation"] = conversation
    report["final_evidence"] = agent.memory.evidence_summary()
    report["all_audit_events"] = agent.audit_events
    report["tool_ok_count"] = sum(1 for e in agent.audit_events if e.get("event") == "tool_ok")
    report["tool_requested_count"] = sum(
        1 for e in agent.audit_events if e.get("event") == "tool_requested"
    )
    report["confirmation_required"] = any(
        e.get("event") == "confirmation_required" for e in agent.audit_events
    )
    report["provider_class_confirmed"] = type(agent._model).__name__  # noqa: SLF001

    # Provider-outage check: point at a dead port, expect bounded failure.
    dead_cfg = AgentConfig(
        provider="local",
        openai_base_url="http://127.0.0.1:59999/v1",
        model_id=model_id,
        request_timeout_seconds=3.0,
        max_iterations=2,
        broker_path=broker_path,
    )
    dead_provider = LocalModelProvider(dead_cfg)
    dead_agent = Agent(model=dead_provider, broker=BrokerClient(dead_cfg.resolve_broker_path()), config=dead_cfg)
    t2 = time.perf_counter()
    outage_reply = dead_agent.handle("اختبار")
    outage_elapsed = time.perf_counter() - t2
    report["provider_outage"] = {
        "reply": outage_reply,
        "elapsed_seconds": round(outage_elapsed, 3),
        "audit_events": dead_agent.audit_events,
        "crashed": False,
    }
    dead_agent._broker.shutdown()  # noqa: SLF001

    report["ended_at"] = _now()
    report["final_gpu"] = _nvidia()
    report["final_ram"] = _ram()

    out_path = EVIDENCE_DIR / "live_qwen_acceptance.json"
    out_path.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"Wrote {out_path}")

    broker.shutdown()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
