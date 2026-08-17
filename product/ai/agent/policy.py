"""Agent-side policy classification.

The Rust broker remains the enforcement authority. This module only mirrors
risk classes so the Agent can present confirmation prompts before sending
sensitive requests, and so forged model claims of approval are ignored.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
from typing import Any


class RiskClass(str, Enum):
    READ = "READ"
    LOW_RISK_ACTION = "LOW_RISK_ACTION"
    SENSITIVE_ACTION = "SENSITIVE_ACTION"
    FORBIDDEN = "FORBIDDEN"


# Canonical tool risk map. Keep aligned with broker/src/registry.rs.
TOOL_RISK: dict[str, RiskClass] = {
    "get_system_info": RiskClass.READ,
    "get_cpu_info": RiskClass.READ,
    "get_memory_info": RiskClass.READ,
    "get_disk_info": RiskClass.READ,
    "list_processes": RiskClass.READ,
    "launch_application": RiskClass.LOW_RISK_ACTION,
    # Explicitly reserved / unavailable in V1.
    "terminate_process": RiskClass.SENSITIVE_ACTION,
    "run_shell": RiskClass.FORBIDDEN,
    "execute_command": RiskClass.FORBIDDEN,
}


@dataclass(frozen=True)
class PolicyDecision:
    allowed: bool
    risk: RiskClass
    requires_confirmation: bool
    reason: str


def classify_tool(tool: str) -> RiskClass:
    return TOOL_RISK.get(tool, RiskClass.FORBIDDEN)


def decide_policy(
    tool: str,
    arguments: dict[str, Any],
    *,
    confirmed: bool = False,
) -> PolicyDecision:
    risk = classify_tool(tool)
    if risk is RiskClass.FORBIDDEN:
        return PolicyDecision(
            allowed=False,
            risk=risk,
            requires_confirmation=False,
            reason=f"tool '{tool}' is forbidden",
        )
    if risk is RiskClass.SENSITIVE_ACTION:
        if not confirmed:
            return PolicyDecision(
                allowed=False,
                risk=risk,
                requires_confirmation=True,
                reason="sensitive action requires explicit user confirmation outside the model",
            )
        # Even with confirmation, terminate_process is not implemented by the
        # broker in V1 — Agent must still refuse execution.
        return PolicyDecision(
            allowed=False,
            risk=risk,
            requires_confirmation=False,
            reason="sensitive action is acknowledged but not implemented by the broker in V1",
        )
    if risk is RiskClass.LOW_RISK_ACTION and not confirmed:
        # Launch is allowlisted and broker-enforced; V1 does not require a
        # second confirmation prompt for allowlisted app launch.
        return PolicyDecision(
            allowed=True,
            risk=risk,
            requires_confirmation=False,
            reason="low-risk allowlisted action",
        )
    return PolicyDecision(
        allowed=True,
        risk=risk,
        requires_confirmation=False,
        reason="read-only telemetry",
    )


SHELL_SHAPED_MARKERS = (
    "rm -rf",
    "sudo ",
    "/bin/sh",
    "/bin/bash",
    "powershell",
    "cmd.exe",
    "&&",
    ";",
    "|",
    "`",
)


def looks_like_shell_payload(arguments: dict[str, Any]) -> bool:
    blob = " ".join(str(v) for v in arguments.values()).lower()
    return any(marker in blob for marker in SHELL_SHAPED_MARKERS)
