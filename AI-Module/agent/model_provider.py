"""ModelProvider abstraction.

The agent core never talks to a specific model API directly — only through
this interface. `RuleBasedProvider` is the deterministic test fallback.
`LocalModelProvider` talks to an OpenAI-compatible local endpoint.
"""

from __future__ import annotations

import json
import logging
import re
import urllib.error
import urllib.request
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from typing import Any, Literal

from config import AgentConfig
from tool_catalog import openai_tool_definitions, validate_arguments

logger = logging.getLogger("tuwaiq_agent.model_provider")


@dataclass
class AgentAction:
    """What the model decided to do for one loop iteration."""

    kind: Literal["respond", "call_tool"]
    text: str | None = None
    tool: str | None = None
    arguments: dict[str, Any] = field(default_factory=dict)


@dataclass
class ProviderFailure:
    """Typed provider failure that must not crash the agent."""

    code: str
    message: str


class ModelProvider(ABC):
    """Interface every model backend implements."""

    @abstractmethod
    def decide(
        self,
        user_message: str,
        *,
        evidence_summary: list[dict[str, Any]] | None = None,
        history: list[dict[str, str]] | None = None,
    ) -> AgentAction | ProviderFailure:
        """Decide whether to answer directly or call exactly one tool."""

    @abstractmethod
    def explain(self, user_message: str, tool: str, result: dict[str, Any]) -> str:
        """Turn a successful tool result into a natural-language answer."""

    @abstractmethod
    def explain_error(
        self, user_message: str, tool: str, error_code: str, error_message: str
    ) -> str:
        """Turn a tool error into an honest user-facing explanation."""

    def finalize(
        self,
        user_message: str,
        evidence_summary: list[dict[str, Any]],
    ) -> str:
        """Synthesize a grounded final answer from collected evidence."""
        if not evidence_summary:
            return (
                "I do not have fresh trusted system evidence for that yet. "
                "Ask me to check CPU, memory, disk, or processes."
            )
        parts: list[str] = []
        for record in evidence_summary:
            parts.append(self.explain(user_message, record["tool"], record["result"]))
        return " ".join(parts)


class RuleBasedProvider(ModelProvider):
    """Deterministic fallback for tests and offline demos. Not a real LLM."""

    _PERFORMANCE_PATTERNS = [
        r"\bslow\b",
        r"\bperformance\b",
        r"how.*(computer|system|pc).*doing",
        r"\bram\b",
        r"\bmemory\b",
        r"\bcpu\b",
        r"\bdisk\b",
        r"\bstorage\b",
        r"\bspace\b",
        # Arabic slow / performance intents
        r"بطيء",
        r"بطيئة",
        r"بطئ",
        r"الأداء",
        r"الاداء",
        r"الذاكرة",
        r"المعالج",
        r"لماذا.*(بطيء|ثقيل)",
    ]
    _PROCESS_PATTERNS = [
        r"\bprocess(es)?\b",
        r"what.*running",
        r"consum(ing|es)",
        r"العمليات",
        r"عملية",
        r"يعمل الآن",
        r"يشغل",
    ]
    _LAUNCH_PATTERNS = [r"\bopen\b", r"\blaunch\b", r"\bstart\b", r"افتح", r"شغّل", r"شغل"]
    _CLOSE_PATTERNS = [
        r"\b(kill|close|terminate|stop|end)\b.*\b(process|app|application|pid)\b",
        r"\b(kill|close|terminate)\b",
        r"أغلق",
        r"اقفل",
        r"أوقف",
        r"اوقف",
        r"أنهِ",
        r"انهِ",
        r"اقتل",
    ]
    _APP_ALIASES = {
        "firefox": "firefox",
        "vscode": "vscode",
        "vs code": "vscode",
        "code": "vscode",
        "terminal": "terminal",
        "file manager": "file_manager",
        "files": "file_manager",
        "فايرفوكس": "firefox",
        "تيرمينال": "terminal",
    }

    def decide(
        self,
        user_message: str,
        *,
        evidence_summary: list[dict[str, Any]] | None = None,
        history: list[dict[str, str]] | None = None,
    ) -> AgentAction | ProviderFailure:
        del history  # unused in rule provider; signature kept for interface parity
        text = user_message.lower().strip()
        evidence_summary = evidence_summary or []
        have_tools = {e["tool"] for e in evidence_summary}

        if any(re.search(p, user_message, flags=re.IGNORECASE) for p in self._CLOSE_PATTERNS):
            focus_name = None
            for record in reversed(evidence_summary):
                result = record.get("result") or {}
                if record["tool"] == "list_processes":
                    procs = result.get("processes") or []
                    if procs:
                        focus_name = procs[0].get("name")
                        break
                if record["tool"] == "get_memory_info":
                    top = result.get("top_consumers") or []
                    if top:
                        focus_name = top[0].get("name")
                        break
            return AgentAction(
                kind="call_tool",
                tool="terminate_process",
                arguments={"name": focus_name or "unknown"},
            )

        launch_match = self._match_launch(text)
        if launch_match is not None:
            return AgentAction(
                kind="call_tool",
                tool="launch_application",
                arguments={"app_id": launch_match},
            )

        # Bounded multi-tool collection for "slow system" style prompts.
        slow_like = any(re.search(p, user_message, flags=re.IGNORECASE) for p in self._PERFORMANCE_PATTERNS)
        if slow_like:
            for needed in ("get_memory_info", "get_cpu_info", "list_processes"):
                if needed not in have_tools:
                    return AgentAction(kind="call_tool", tool=needed)
            return AgentAction(kind="respond", text=None)

        if any(re.search(p, user_message, flags=re.IGNORECASE) for p in self._PROCESS_PATTERNS):
            if "list_processes" not in have_tools:
                return AgentAction(kind="call_tool", tool="list_processes")
            return AgentAction(kind="respond", text=None)

        if "disk" in text or "space" in text or "storage" in text or "القرص" in user_message:
            if "get_disk_info" not in have_tools:
                return AgentAction(kind="call_tool", tool="get_disk_info")
            return AgentAction(kind="respond", text=None)

        if "system" in text or "hostname" in text or "kernel" in text or "uptime" in text:
            if "get_system_info" not in have_tools:
                return AgentAction(kind="call_tool", tool="get_system_info")
            return AgentAction(kind="respond", text=None)

        # Follow-up that reuses evidence: if user asks about the top consumer
        # and we already have memory/process evidence, respond from evidence.
        if evidence_summary and (
            "نفس" in user_message
            or "السابق" in user_message
            or "again" in text
            or "still" in text
            or "what about" in text
            or "ماذا عن" in user_message
            or "هل ما زال" in user_message
        ):
            return AgentAction(kind="respond", text=None)

        return AgentAction(
            kind="respond",
            text=(
                "I can check your system info, CPU, memory, disk, running processes, "
                "or open an approved application (Firefox, VS Code, Terminal, File Manager). "
                "What would you like to know?"
            ),
        )

    def _match_launch(self, text: str) -> str | None:
        if not any(re.search(p, text) for p in self._LAUNCH_PATTERNS):
            return None
        for alias, app_id in self._APP_ALIASES.items():
            if alias in text:
                return app_id
        return None

    def explain(self, user_message: str, tool: str, result: dict[str, Any]) -> str:
        del user_message
        if tool == "get_memory_info":
            top = result.get("top_consumers") or []
            top_line = ""
            if top:
                first = top[0]
                gb = first["bytes"] / (1024**3)
                top_line = f" The biggest consumer is {first['name']} at {gb:.1f} GB."
            return f"Memory usage is at {result['used_percent']:.1f}%.{top_line}"

        if tool == "get_cpu_info":
            return (
                f"CPU usage is currently {result['usage_percent']:.1f}% "
                f"across {result['core_count']} cores."
            )

        if tool == "get_disk_info":
            lines = [
                f"{v['mount_point']}: {v['used_percent']:.1f}% used"
                for v in result.get("volumes", [])
            ]
            return "Disk usage — " + "; ".join(lines) if lines else "No disk volumes found."

        if tool == "list_processes":
            top = result.get("processes", [])[:3]
            names = ", ".join(f"{p['name']} ({p['cpu_percent']:.1f}% CPU)" for p in top)
            return f"Top processes right now: {names}." if names else "No process data available."

        if tool == "get_system_info":
            return (
                f"You're running {result['os_name']} {result['os_version']} "
                f"(kernel {result['kernel_version']}) on host '{result['hostname']}', "
                f"up for {result['uptime_seconds']} seconds."
            )

        if tool == "launch_application":
            return f"Opened {result['app_id']} (pid {result['pid']})."

        return f"Done: {result}"

    def explain_error(
        self, user_message: str, tool: str, error_code: str, error_message: str
    ) -> str:
        del user_message, tool
        if error_code == "not_allowlisted":
            return (
                "I can only open a small set of approved applications "
                "(Firefox, VS Code, Terminal, File Manager) — that app isn't one of them."
            )
        if error_code == "invalid_arguments":
            return "I wasn't able to form a valid request for that — could you rephrase?"
        if error_code == "permission_denied":
            return "That action was denied by policy."
        if error_code == "internal_error":
            return "Something went wrong talking to the system tools. Please try again in a moment."
        return f"I couldn't complete that: {error_message}"


class LocalModelProvider(ModelProvider):
    """OpenAI-compatible local chat-completions provider (Ollama/vLLM/SGLang/etc.).

    Boundedness: timeouts, max context chars, structured tool_calls only.
    Malformed / unknown tool calls become ProviderFailure, never crashes.
    """

    def __init__(self, config: AgentConfig | None = None):
        self._config = config or AgentConfig.from_environ()
        self._tools = openai_tool_definitions()

    def decide(
        self,
        user_message: str,
        *,
        evidence_summary: list[dict[str, Any]] | None = None,
        history: list[dict[str, str]] | None = None,
    ) -> AgentAction | ProviderFailure:
        messages = self._build_messages(user_message, evidence_summary, history)
        payload = {
            "model": self._config.model_id,
            "messages": messages,
            "tools": self._tools,
            "tool_choice": "auto",
            "temperature": 0.1,
        }
        try:
            data = self._post_chat(payload)
        except TimeoutError:
            return ProviderFailure(code="provider_timeout", message="local model request timed out")
        except urllib.error.URLError as exc:
            return ProviderFailure(
                code="provider_unavailable",
                message=f"local model endpoint unreachable: {exc.reason}",
            )
        except Exception as exc:  # noqa: BLE001 - must never crash the agent
            logger.exception("local provider request failed")
            return ProviderFailure(code="provider_error", message=str(exc))

        return self._parse_completion(data)

    def explain(self, user_message: str, tool: str, result: dict[str, Any]) -> str:
        # Prefer a short local synthesis request; fall back to rule formatting
        # if the endpoint is unavailable mid-session.
        fallback = RuleBasedProvider().explain(user_message, tool, result)
        payload = {
            "model": self._config.model_id,
            "messages": [
                {
                    "role": "system",
                    "content": (
                        "Explain the trusted tool evidence briefly for the user. "
                        "Do not invent facts beyond the JSON evidence."
                    ),
                },
                {
                    "role": "user",
                    "content": json.dumps(
                        {"user_message": user_message, "tool": tool, "result": result},
                        ensure_ascii=False,
                    ),
                },
            ],
            "temperature": 0.1,
        }
        try:
            data = self._post_chat(payload)
            text = self._message_text(data)
            return text or fallback
        except Exception:
            return fallback

    def explain_error(
        self, user_message: str, tool: str, error_code: str, error_message: str
    ) -> str:
        return RuleBasedProvider().explain_error(user_message, tool, error_code, error_message)

    def finalize(
        self,
        user_message: str,
        evidence_summary: list[dict[str, Any]],
    ) -> str:
        if not evidence_summary:
            return RuleBasedProvider().finalize(user_message, evidence_summary)
        payload = {
            "model": self._config.model_id,
            "messages": [
                {
                    "role": "system",
                    "content": (
                        "You are Tuwaiq OS assistant. Answer using ONLY the trusted "
                        "evidence JSON. Distinguish evidence from user claims. "
                        "Reply in the user's language when possible."
                    ),
                },
                {
                    "role": "user",
                    "content": json.dumps(
                        {
                            "user_message": user_message,
                            "trusted_evidence": evidence_summary,
                        },
                        ensure_ascii=False,
                    ),
                },
            ],
            "temperature": 0.1,
        }
        try:
            data = self._post_chat(payload)
            text = self._message_text(data)
            if text:
                return text
        except Exception:
            logger.exception("finalize via local model failed; using rule fallback")
        return RuleBasedProvider().finalize(user_message, evidence_summary)

    def _build_messages(
        self,
        user_message: str,
        evidence_summary: list[dict[str, Any]] | None,
        history: list[dict[str, str]] | None,
    ) -> list[dict[str, Any]]:
        system = (
            "You are the Tuwaiq local system assistant. "
            "Use tools for live system state. Never invent process/CPU/memory facts. "
            "Never claim approval for sensitive actions. "
            "Never ask for or run shell commands. "
            "If evidence is already provided and sufficient, answer without more tools. "
            "Gulf/Arabic close intents such as سكره، سكر، أغلق، اقفل، أوقف about a "
            "process/app mean request terminate_process for the focused process from "
            "trusted evidence (name/pid). Do not confuse سكره with screen/display settings. "
            "Never claim a process was killed; the broker will not execute termination in V1."
        )
        messages: list[dict[str, Any]] = [{"role": "system", "content": system}]
        if history:
            for turn in history[-8:]:
                messages.append({"role": turn["role"], "content": turn["content"]})
        if evidence_summary:
            messages.append(
                {
                    "role": "system",
                    "content": "Trusted broker evidence (fresh only):\n"
                    + json.dumps(evidence_summary, ensure_ascii=False),
                }
            )
        messages.append({"role": "user", "content": user_message})
        blob = json.dumps(messages, ensure_ascii=False)
        if len(blob) > self._config.max_context_chars:
            # Drop oldest history first.
            while len(messages) > 3 and len(json.dumps(messages, ensure_ascii=False)) > self._config.max_context_chars:
                # Keep system + optional evidence + user; drop middle history.
                del messages[1]
        return messages

    def _post_chat(self, payload: dict[str, Any]) -> dict[str, Any]:
        url = f"{self._config.openai_base_url}/chat/completions"
        body = json.dumps(payload).encode("utf-8")
        req = urllib.request.Request(
            url,
            data=body,
            method="POST",
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {self._config.openai_api_key}",
            },
        )
        with urllib.request.urlopen(req, timeout=self._config.request_timeout_seconds) as resp:
            raw = resp.read().decode("utf-8")
        return json.loads(raw)

    def _parse_completion(self, data: dict[str, Any]) -> AgentAction | ProviderFailure:
        try:
            choices = data.get("choices") or []
            if not choices:
                return ProviderFailure(code="malformed_response", message="no choices in completion")
            message = choices[0].get("message") or {}
            tool_calls = message.get("tool_calls") or []
            if tool_calls:
                call = tool_calls[0]
                fn = call.get("function") or {}
                name = fn.get("name")
                if not isinstance(name, str) or not name:
                    return ProviderFailure(code="malformed_tool_call", message="missing tool name")
                raw_args = fn.get("arguments", "{}")
                if isinstance(raw_args, dict):
                    arguments = raw_args
                else:
                    try:
                        arguments = json.loads(raw_args or "{}")
                    except json.JSONDecodeError:
                        return ProviderFailure(
                            code="malformed_tool_call",
                            message="tool arguments were not valid JSON",
                        )
                if not isinstance(arguments, dict):
                    return ProviderFailure(
                        code="malformed_tool_call",
                        message="tool arguments must be an object",
                    )
                err = validate_arguments(name, arguments)
                # Unknown tools still returned as call_tool so agent policy can refuse;
                # only schema malformation of known/reserved names fails here when args bad.
                if err and name in {t["function"]["name"] for t in self._tools}:
                    return ProviderFailure(code="invalid_tool_arguments", message=err)
                return AgentAction(kind="call_tool", tool=name, arguments=arguments)

            text = message.get("content")
            if isinstance(text, str) and text.strip():
                return AgentAction(kind="respond", text=text.strip())
            # Empty content with no tools: ask agent to finalize from evidence.
            return AgentAction(kind="respond", text=None)
        except Exception as exc:  # noqa: BLE001
            logger.exception("failed to parse local model completion")
            return ProviderFailure(code="malformed_response", message=str(exc))

    @staticmethod
    def _message_text(data: dict[str, Any]) -> str | None:
        choices = data.get("choices") or []
        if not choices:
            return None
        content = (choices[0].get("message") or {}).get("content")
        if isinstance(content, str) and content.strip():
            return content.strip()
        return None


def build_provider(config: AgentConfig | None = None) -> ModelProvider:
    cfg = config or AgentConfig.from_environ()
    if cfg.provider in {"rule", "rules", "rulebased", "test"}:
        return RuleBasedProvider()
    return LocalModelProvider(cfg)
