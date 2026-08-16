"""Canonical tool catalog shared with the Rust broker authority.

The broker remains the enforcement point. This module exists so the Python
agent can validate schemas and present tool definitions to a local model
without inventing a divergent tool list. Keep aligned with
`broker/src/registry.rs`.
"""

from __future__ import annotations

from typing import Any

from policy import RiskClass

TOOL_CATALOG: dict[str, dict[str, Any]] = {
    "get_system_info": {
        "risk": RiskClass.READ.value,
        "description": "Read host OS name, version, kernel, hostname, and uptime.",
        "parameters": {"type": "object", "properties": {}, "additionalProperties": False},
    },
    "get_cpu_info": {
        "risk": RiskClass.READ.value,
        "description": "Read CPU model, core count, and usage percent.",
        "parameters": {"type": "object", "properties": {}, "additionalProperties": False},
    },
    "get_memory_info": {
        "risk": RiskClass.READ.value,
        "description": "Read memory totals/usage and top memory consumers.",
        "parameters": {"type": "object", "properties": {}, "additionalProperties": False},
    },
    "get_disk_info": {
        "risk": RiskClass.READ.value,
        "description": "Read mounted volume capacity and usage.",
        "parameters": {"type": "object", "properties": {}, "additionalProperties": False},
    },
    "list_processes": {
        "risk": RiskClass.READ.value,
        "description": "List top processes by CPU/memory (bounded).",
        "parameters": {"type": "object", "properties": {}, "additionalProperties": False},
    },
    "launch_application": {
        "risk": RiskClass.LOW_RISK_ACTION.value,
        "description": "Launch an allowlisted application by fixed app_id only.",
        "parameters": {
            "type": "object",
            "properties": {
                "app_id": {
                    "type": "string",
                    "enum": ["firefox", "vscode", "terminal", "file_manager"],
                }
            },
            "required": ["app_id"],
            "additionalProperties": False,
        },
    },
}

# Sensitive intents the broker does not execute in V1. Present for policy
# detection / confirmation UX only — never registered as callable tools.
RESERVED_SENSITIVE_TOOLS: dict[str, dict[str, Any]] = {
    "terminate_process": {
        "risk": RiskClass.SENSITIVE_ACTION.value,
        "description": (
            "Request ending/closing a running process by pid and/or name. "
            "Use this when the user asks to close, stop, kill, or سكر/سكره a process. "
            "This only submits a sensitive request; never claim it already executed."
        ),
        "parameters": {
            "type": "object",
            "properties": {
                "pid": {"type": "integer"},
                "name": {"type": "string"},
            },
            "additionalProperties": False,
        },
    },
}

FORBIDDEN_TOOLS = frozenset({"run_shell", "execute_command", "shell", "bash"})


def openai_tool_definitions() -> list[dict[str, Any]]:
    """OpenAI-compatible tool schema list for local chat-completions APIs.

    Includes reserved sensitive intents so the model can *request* them; the
    Agent/broker still refuse execution and require external confirmation.
    """
    tools: list[dict[str, Any]] = []
    for name, meta in {**TOOL_CATALOG, **RESERVED_SENSITIVE_TOOLS}.items():
        tools.append(
            {
                "type": "function",
                "function": {
                    "name": name,
                    "description": meta["description"],
                    "parameters": meta["parameters"],
                },
            }
        )
    return tools


def validate_arguments(tool: str, arguments: dict[str, Any]) -> str | None:
    """Return an error string if arguments violate the catalog schema."""
    if tool in FORBIDDEN_TOOLS:
        return f"tool '{tool}' is forbidden"
    meta = TOOL_CATALOG.get(tool) or RESERVED_SENSITIVE_TOOLS.get(tool)
    if meta is None:
        return f"unknown tool '{tool}'"
    params = meta["parameters"]
    props = params.get("properties") or {}
    required = set(params.get("required") or [])
    additional = params.get("additionalProperties", True)

    if not isinstance(arguments, dict):
        return "arguments must be an object"

    missing = [key for key in required if key not in arguments]
    if missing:
        return f"missing required argument(s): {', '.join(missing)}"

    if additional is False:
        extras = [key for key in arguments if key not in props]
        if extras:
            return f"unexpected argument(s): {', '.join(extras)}"

    for key, value in arguments.items():
        if key not in props:
            continue
        schema = props[key]
        expected = schema.get("type")
        if expected == "string" and not isinstance(value, str):
            return f"'{key}' must be a string"
        if expected == "integer" and not isinstance(value, int):
            return f"'{key}' must be an integer"
        enum = schema.get("enum")
        if enum is not None and value not in enum:
            return f"'{key}' must be one of {enum}"
    return None
