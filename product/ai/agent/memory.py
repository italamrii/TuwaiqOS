"""Trusted evidence and bounded session memory.

User statements and model prose are untrusted. Only broker tool results are
trusted live-system evidence, and only while fresh.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any, Literal


def _now() -> datetime:
    return datetime.now(timezone.utc)


@dataclass
class EvidenceRecord:
    tool: str
    result: dict[str, Any]
    request_id: str
    collected_at: datetime
    user_message: str


@dataclass
class TurnRecord:
    role: Literal["user", "assistant", "system"]
    content: str
    trusted: bool = False


@dataclass
class SessionMemory:
    max_turns: int = 32
    max_evidence: int = 24
    evidence_ttl_seconds: float = 120.0
    turns: list[TurnRecord] = field(default_factory=list)
    evidence: list[EvidenceRecord] = field(default_factory=list)
    # External confirmation state — never derived from model text alone.
    pending_confirmation: dict[str, Any] | None = None
    last_focus_process: dict[str, Any] | None = None

    def add_user(self, text: str) -> None:
        self._append_turn(TurnRecord(role="user", content=text, trusted=False))

    def add_assistant(self, text: str) -> None:
        self._append_turn(TurnRecord(role="assistant", content=text, trusted=False))

    def add_evidence(
        self,
        *,
        tool: str,
        result: dict[str, Any],
        request_id: str,
        user_message: str,
    ) -> EvidenceRecord:
        record = EvidenceRecord(
            tool=tool,
            result=result,
            request_id=request_id,
            collected_at=_now(),
            user_message=user_message,
        )
        self.evidence.append(record)
        if len(self.evidence) > self.max_evidence:
            self.evidence = self.evidence[-self.max_evidence :]
        self._update_focus_from_evidence(record)
        return record

    def fresh_evidence(self) -> list[EvidenceRecord]:
        now = _now()
        out: list[EvidenceRecord] = []
        for record in self.evidence:
            age = (now - record.collected_at).total_seconds()
            if age <= self.evidence_ttl_seconds:
                out.append(record)
        return out

    def evidence_summary(self) -> list[dict[str, Any]]:
        summary: list[dict[str, Any]] = []
        for record in self.fresh_evidence():
            summary.append(
                {
                    "tool": record.tool,
                    "request_id": record.request_id,
                    "collected_at": record.collected_at.isoformat(),
                    "result": record.result,
                }
            )
        return summary

    def has_system_evidence(self) -> bool:
        return any(
            r.tool
            in {
                "get_system_info",
                "get_cpu_info",
                "get_memory_info",
                "get_disk_info",
                "list_processes",
            }
            for r in self.fresh_evidence()
        )

    def set_pending_confirmation(self, payload: dict[str, Any]) -> None:
        self.pending_confirmation = dict(payload)

    def clear_pending_confirmation(self) -> None:
        self.pending_confirmation = None

    def _append_turn(self, turn: TurnRecord) -> None:
        self.turns.append(turn)
        if len(self.turns) > self.max_turns:
            self.turns = self.turns[-self.max_turns :]

    def _update_focus_from_evidence(self, record: EvidenceRecord) -> None:
        if record.tool == "list_processes":
            procs = record.result.get("processes") or []
            if procs:
                self.last_focus_process = dict(procs[0])
        elif record.tool == "get_memory_info":
            top = record.result.get("top_consumers") or []
            if top:
                first = dict(top[0])
                # Normalize to a common focus shape.
                self.last_focus_process = {
                    "pid": first.get("pid"),
                    "name": first.get("name"),
                    "memory_bytes": first.get("bytes"),
                }
