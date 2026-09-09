"""Safe parser and validator for Qwen structured tool call output (Phase 3).

Qwen is instructed to emit tool calls as JSON objects of the form:
  {"tool": "<name>", "arguments": { ... }}

This module:
1. Extracts and parses that JSON from raw model output.
2. Validates the structure and argument schema before anything reaches the
   Rust broker.
3. Returns a typed ParsedToolCall on success, or raises a descriptive
   ToolCallError that the caller converts to a safe user-facing message.

Security guarantees upheld here:
- Only tool names in KNOWN_TOOLS are accepted (all others → ToolCallError).
- Argument validation uses the per-tool JSON schema from tool_schemas.py --
  no extra keys, no shell strings, no arbitrary commands.
- Model output that resembles a shell command (e.g. "run: rm -rf /") is
  detected and rejected explicitly.
- Raw model text that cannot be parsed as JSON is rejected (not executed).
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from typing import Any

from protocol import KNOWN_TOOLS
from tool_schemas import TOOL_SCHEMA_BY_NAME


# ---------------------------------------------------------------------------
# Public exceptions
# ---------------------------------------------------------------------------

class ToolCallError(ValueError):
    """Base class for all tool-call validation failures."""


class MalformedToolCallError(ToolCallError):
    """Model output could not be parsed as a valid JSON tool call."""


class UnknownToolError(ToolCallError):
    """Model requested a tool that is not in the registry."""


class InvalidArgumentsError(ToolCallError):
    """Model supplied arguments that fail schema validation for the tool."""


class ShellCommandAttemptError(ToolCallError):
    """Model output resembles a shell command, which is never executable here."""


# ---------------------------------------------------------------------------
# Data container
# ---------------------------------------------------------------------------

@dataclass
class ParsedToolCall:
    tool: str
    arguments: dict[str, Any] = field(default_factory=dict)


# ---------------------------------------------------------------------------
# Shell-command detection
# ---------------------------------------------------------------------------

# Patterns that suggest the model is trying to emit a runnable command rather
# than a structured tool call.  These are rejected outright.
_SHELL_PATTERNS: list[re.Pattern[str]] = [
    re.compile(r"^\s*run\s*:", re.IGNORECASE),
    re.compile(r"^\s*exec\s*:", re.IGNORECASE),
    re.compile(r"^\s*\$\s+"),
    re.compile(r"^\s*bash\s*:", re.IGNORECASE),
    re.compile(r"^\s*sh\s+-c\s+", re.IGNORECASE),
    re.compile(r"^\s*os\.system\s*\(", re.IGNORECASE),
    re.compile(r"^\s*subprocess\.", re.IGNORECASE),
    re.compile(r"rm\s+-rf", re.IGNORECASE),
    re.compile(r"sudo\s+", re.IGNORECASE),
]


def _looks_like_shell_command(text: str) -> bool:
    return any(p.search(text) for p in _SHELL_PATTERNS)


def is_shell_command_attempt(text: str) -> bool:
    """Return True when *text* (or the parts of it outside any JSON block)
    looks like a shell command rather than a structured tool call or plain NL."""
    text_outside_json = _JSON_BLOCK.sub("", text)
    return _looks_like_shell_command(text_outside_json)


# ---------------------------------------------------------------------------
# JSON extraction
# ---------------------------------------------------------------------------

# Matches the first {...} block in the text, accounting for nested braces.
_JSON_BLOCK = re.compile(r"\{.*\}", re.DOTALL)


def _extract_json(text: str) -> str | None:
    """Extract the first JSON object found in *text*, or None."""
    m = _JSON_BLOCK.search(text)
    return m.group(0) if m else None


# ---------------------------------------------------------------------------
# Schema validation (lightweight, no external library required)
# ---------------------------------------------------------------------------

def _validate_arguments(tool_name: str, arguments: Any) -> None:
    """Validate *arguments* against the schema for *tool_name*.

    Raises InvalidArgumentsError on any violation.  The validation is
    intentionally strict: extra keys, wrong types, and missing required
    keys are all rejected.
    """
    if not isinstance(arguments, dict):
        raise InvalidArgumentsError(
            f"tool '{tool_name}' arguments must be a JSON object, got {type(arguments).__name__}"
        )

    schema = TOOL_SCHEMA_BY_NAME.get(tool_name)
    if schema is None:
        # This should be caught by the KNOWN_TOOLS check before we get here,
        # but guard anyway.
        raise UnknownToolError(f"no schema registered for tool '{tool_name}'")

    params = schema.get("parameters", {})
    required: list[str] = params.get("required", [])
    allowed_props: dict[str, Any] = params.get("properties", {})
    additional_ok: bool = params.get("additionalProperties", True)

    # Check required keys.
    for key in required:
        if key not in arguments:
            raise InvalidArgumentsError(
                f"tool '{tool_name}' requires argument '{key}' which is missing"
            )

    # Check for extra keys when additionalProperties is False.
    if not additional_ok:
        for key in arguments:
            if key not in allowed_props:
                raise InvalidArgumentsError(
                    f"tool '{tool_name}' does not accept argument '{key}'"
                )

    # Type-check individual properties.
    for key, value in arguments.items():
        prop_schema = allowed_props.get(key)
        if prop_schema is None:
            continue  # covered by additionalProperties check above
        expected_type = prop_schema.get("type")
        if expected_type == "string" and not isinstance(value, str):
            raise InvalidArgumentsError(
                f"tool '{tool_name}' argument '{key}' must be a string"
            )
        if expected_type == "integer" and not isinstance(value, int):
            raise InvalidArgumentsError(
                f"tool '{tool_name}' argument '{key}' must be an integer"
            )
        if expected_type == "number" and not isinstance(value, (int, float)):
            raise InvalidArgumentsError(
                f"tool '{tool_name}' argument '{key}' must be a number"
            )
        if expected_type == "boolean" and not isinstance(value, bool):
            raise InvalidArgumentsError(
                f"tool '{tool_name}' argument '{key}' must be a boolean"
            )
        # Enum check.
        allowed_values = prop_schema.get("enum")
        if allowed_values is not None and value not in allowed_values:
            raise InvalidArgumentsError(
                f"tool '{tool_name}' argument '{key}' must be one of "
                f"{allowed_values!r}, got {value!r}"
            )


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

def parse_tool_call(model_output: str) -> ParsedToolCall:
    """Parse and validate a structured tool call from *model_output*.

    Returns a ParsedToolCall on success.
    Raises a ToolCallError subclass (never executes anything) on any failure.
    """
    raw_json = _extract_json(model_output)

    if raw_json is None:
        # No JSON at all -- check whether this is a raw shell command string.
        if _looks_like_shell_command(model_output):
            raise ShellCommandAttemptError(
                "model output resembles a shell command and will never be executed"
            )
        raise MalformedToolCallError("no JSON object found in model output")

    # Reject output that has a shell command prefix *outside* the JSON block
    # (e.g. "run: {...}").  A "sudo" inside a JSON string value is handled
    # by argument validation (enum/schema check) further below.
    text_outside_json = _JSON_BLOCK.sub("", model_output)
    if _looks_like_shell_command(text_outside_json):
        raise ShellCommandAttemptError(
            "model output has a shell command prefix outside the JSON tool call"
        )

    try:
        data = json.loads(raw_json)
    except json.JSONDecodeError as exc:
        raise MalformedToolCallError(f"could not parse JSON from model output: {exc}") from exc

    if not isinstance(data, dict):
        raise MalformedToolCallError("parsed JSON is not an object")

    tool_name = data.get("tool")
    if not isinstance(tool_name, str) or not tool_name.strip():
        raise MalformedToolCallError("tool call JSON missing or empty 'tool' field")

    tool_name = tool_name.strip()
    if tool_name not in KNOWN_TOOLS:
        raise UnknownToolError(f"model requested unknown tool '{tool_name}'")

    arguments = data.get("arguments", {})
    _validate_arguments(tool_name, arguments)

    return ParsedToolCall(tool=tool_name, arguments=arguments)


def is_tool_call(model_output: str) -> bool:
    """Return True when *model_output* looks like a structured tool call.

    Does not raise -- use parse_tool_call() to get the validated result or
    the specific error.
    """
    raw_json = _extract_json(model_output)
    if raw_json is None:
        return False
    # Reject shell command prefixes *outside* the JSON block.
    text_outside_json = _JSON_BLOCK.sub("", model_output)
    if _looks_like_shell_command(text_outside_json):
        return False
    try:
        data = json.loads(raw_json)
    except json.JSONDecodeError:
        return False
    return isinstance(data, dict) and "tool" in data
