"""Tuwaiq Agent Broker Protocol v1 -- Python side.

Mirrors protocol/schema.json and broker/src/protocol.rs exactly. This is the
only module in the Python codebase that knows the wire format; everything
else (agent.py, tools.py) talks in terms of these types, not raw dicts.
"""

from __future__ import annotations

import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any

PROTOCOL_VERSION = "1.0"

# The exact, closed set of tool names the broker will accept. Kept here too
# (not just in the broker) so the Python side can fail fast locally on a
# typo'd tool name before ever forming a request.
KNOWN_TOOLS = frozenset(
    {
        "get_system_info",
        "get_cpu_info",
        "get_memory_info",
        "get_disk_info",
        "list_processes",
        "get_network_status",
        "kill_process",
        "close_application",
        "launch_application",
    }
)


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


@dataclass
class ToolRequest:
    tool: str
    arguments: dict[str, Any] = field(default_factory=dict)
    protocol_version: str = PROTOCOL_VERSION
    request_id: str = field(default_factory=lambda: str(uuid.uuid4()))
    timestamp: str = field(default_factory=_now_iso)

    def to_wire_dict(self) -> dict[str, Any]:
        return {
            "protocol_version": self.protocol_version,
            "request_id": self.request_id,
            "timestamp": self.timestamp,
            "tool": self.tool,
            "arguments": self.arguments,
        }


@dataclass
class ToolResponse:
    protocol_version: str
    request_id: str
    timestamp: str
    status: str  # "ok" | "error"
    result: dict[str, Any] | None = None
    error: dict[str, Any] | None = None

    @property
    def ok(self) -> bool:
        return self.status == "ok"

    @property
    def error_code(self) -> str | None:
        return self.error.get("code") if self.error else None

    @property
    def error_message(self) -> str | None:
        return self.error.get("message") if self.error else None

    @staticmethod
    def from_wire_dict(data: dict[str, Any]) -> "ToolResponse":
        return ToolResponse(
            protocol_version=data.get("protocol_version", ""),
            request_id=data.get("request_id", ""),
            timestamp=data.get("timestamp", ""),
            status=data.get("status", "error"),
            result=data.get("result"),
            error=data.get("error"),
        )
