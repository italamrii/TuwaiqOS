"""ModelProvider abstraction.

The agent core (agent.py) never talks to a specific model API directly --
only through this interface. Swapping a local model, a remote provider, or
(later) a Saudi/enterprise-hosted model for the underlying reasoning never
requires touching agent.py, broker_client.py, or the protocol.

    Tuwaiq AI
       |
       +-- ModelProvider (this file's interface)
       |      |
       |      +-- RuleBasedProvider   (default here: no external deps,
       |      |                        deterministic, used by tests/demo)
       |      +-- LocalModelProvider  (local Qwen profile/runtime wiring)
       |      +-- RemoteModelProvider (stub: wire up a hosted API)
       |
       +-- BrokerClient -> Rust broker

`RuleBasedProvider` exists so this whole prototype is runnable and testable
end-to-end without requiring an API key or a multi-GB local model download --
it is intentionally simple (keyword/intent matching), not a real language
model. Swap it for `LocalModelProvider`/`RemoteModelProvider` for actual
natural-language understanding; nothing else in the codebase changes.

Phase 4 adds `decide_next` and `synthesize` for the multi-step agent loop.
Both have default implementations here so no existing subclass is broken.
"""

from __future__ import annotations

import re
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Any, Literal

from local_model_runtime import LocalModelRuntime, LocalRuntimeError, QwenLocalRuntime
from model_profiles import ModelProfile, load_model_profile, resolve_model_path
from protocol import KNOWN_TOOLS

if TYPE_CHECKING:
    from conversation_context import ConversationContext, ToolResultEntry


@dataclass
class AgentAction:
    """What the model decided to do in response to one turn."""

    kind: Literal["respond", "call_tool"]
    # Present when kind == "respond": final text to show the user directly,
    # no tool call needed (e.g. "hi", "what can you do").
    text: str | None = None
    # Present when kind == "call_tool":
    tool: str | None = None
    arguments: dict[str, Any] = field(default_factory=dict)


class ModelProvider(ABC):
    """Interface every model backend implements.

    Core responsibilities:
    - `decide`: single-turn tool-or-respond decision (Phase 1-3).
    - `explain` / `explain_error`: turn a tool result into natural language.
    - `decide_next`: multi-step loop decision given accumulated results (Ph 4).
    - `synthesize`: produce a final answer from multiple tool results (Ph 4).

    `decide_next` and `synthesize` have concrete default implementations here
    so existing subclasses need not change.
    """

    @abstractmethod
    def decide(self, user_message: str) -> AgentAction:
        """Given the user's message, decide whether to answer directly or
        call exactly one tool. Phase 1 is single-tool-per-turn by design --
        multi-step planning is out of scope until the protocol/broker
        support has been exercised enough to trust it."""

    @abstractmethod
    def explain(self, user_message: str, tool: str, result: dict[str, Any]) -> str:
        """Turn a successful tool result into a natural-language answer for
        the user."""

    @abstractmethod
    def explain_error(self, user_message: str, tool: str, error_code: str, error_message: str) -> str:
        """Turn a tool error into an honest, non-technical explanation for
        the user -- never expose raw error codes or internals to them
        directly; that's what the audit log and logger are for."""

    # ------------------------------------------------------------------
    # Phase 4: multi-step loop support (non-abstract; override for richer
    # behaviour)
    # ------------------------------------------------------------------

    def decide_next(
        self,
        user_message: str,
        accumulated: "list[ToolResultEntry]",
        context: "ConversationContext",
    ) -> AgentAction:
        """Decide the next action given already-accumulated tool results.

        Default: delegates to `decide(user_message)`, ignoring accumulated
        results and context.  This preserves Phase 1-3 single-tool behaviour
        when not overridden.
        """
        return self.decide(user_message)

    def synthesize(
        self,
        user_message: str,
        accumulated: "list[ToolResultEntry]",
        context: "ConversationContext",
    ) -> str:
        """Produce a final natural-language answer from multiple tool results.

        Default: explains the last successful result (single-tool fallback),
        or the last error if nothing succeeded.
        """
        if not accumulated:
            return "I don't have enough information to answer that."
        best = next((e for e in reversed(accumulated) if e.ok), accumulated[-1])
        if best.ok:
            return self.explain(user_message, best.tool, best.result)
        return self.explain_error(
            user_message,
            best.tool,
            "internal_error",
            f"tool {best.tool!r} did not return a result",
        )


class LocalModelProvider(ModelProvider):
    """Local-model provider for Qwen runtime integration.

    Keeps agent architecture model-agnostic while preserving a deterministic
    fallback path for tool-routing and tests.
    """

    def __init__(
        self,
        profile: str | ModelProfile = "default",
        runtime: LocalModelRuntime | None = None,
        fallback: ModelProvider | None = None,
        require_model_file: bool = False,
    ) -> None:
        self.profile = load_model_profile(profile)
        self.runtime = runtime or QwenLocalRuntime()
        self._fallback = fallback or RuleBasedProvider()
        self._repo_root = Path(__file__).resolve().parent.parent
        self.runtime.validate(self.profile, root=self._repo_root)
        if require_model_file:
            model_path = resolve_model_path(self.profile, self._repo_root)
            if not model_path.exists():
                raise ValueError(f"model file does not exist: {model_path}")

    def initialize(self) -> None:
        self.runtime.initialize(self.profile, root=self._repo_root)

    def shutdown(self) -> None:
        self.runtime.shutdown()

    def telemetry(self) -> dict[str, Any]:
        return self.runtime.telemetry()

    def decide(self, user_message: str) -> AgentAction:
        # Phase 3: try the Qwen runtime first -- it may emit a structured
        # tool call or a natural-language response.  Fall back to the
        # rule-based provider only when the runtime is unavailable or fails.
        try:
            action = self.runtime.decide(user_message=user_message, profile=self.profile)
        except LocalRuntimeError:
            action = None

        if action is not None:
            return action

        return self._fallback.decide(user_message)

    def decide_next(
        self,
        user_message: str,
        accumulated: "list[ToolResultEntry]",
        context: "ConversationContext",
    ) -> AgentAction:
        try:
            action = self.runtime.decide_next(
                user_message=user_message,
                accumulated=accumulated,
                context=context,
                profile=self.profile,
            )
        except LocalRuntimeError:
            action = None

        if action is not None:
            return action

        return self._fallback.decide_next(user_message, accumulated, context)

    def synthesize(
        self,
        user_message: str,
        accumulated: "list[ToolResultEntry]",
        context: "ConversationContext",
    ) -> str:
        try:
            result = self.runtime.synthesize(
                user_message=user_message,
                accumulated=accumulated,
                context=context,
                profile=self.profile,
            )
        except LocalRuntimeError:
            result = None

        if result is not None:
            return result

        return self._fallback.synthesize(user_message, accumulated, context)

    def explain(self, user_message: str, tool: str, result: dict[str, Any]) -> str:
        try:
            explanation = self.runtime.explain(
                user_message=user_message,
                tool=tool,
                result=result,
                profile=self.profile,
            )
        except LocalRuntimeError:
            explanation = None
        if explanation is not None:
            return explanation
        return self._fallback.explain(user_message, tool, result)

    def explain_error(self, user_message: str, tool: str, error_code: str, error_message: str) -> str:
        try:
            explanation = self.runtime.explain_error(
                user_message=user_message,
                tool=tool,
                error_code=error_code,
                error_message=error_message,
                profile=self.profile,
            )
        except LocalRuntimeError:
            explanation = None
        if explanation is not None:
            return explanation
        return self._fallback.explain_error(user_message, tool, error_code, error_message)


class RuleBasedProvider(ModelProvider):
    """Deterministic, dependency-free provider used as the default so the
    whole system runs without an API key or a local model file. Intent
    matching here is intentionally simple pattern matching, not a
    real language model -- replace with LocalModelProvider or
    RemoteModelProvider for actual natural-language understanding.
    """

    _PERFORMANCE_PATTERNS = [
        r"\bslow\b", r"\bperformance\b", r"how.*(computer|system|pc).*doing",
        r"\bram\b", r"\bmemory\b", r"\bcpu\b", r"\bdisk\b", r"\bstorage\b",
        r"\bspace\b",
    ]
    _PROCESS_PATTERNS = [r"\bprocess(es)?\b", r"what.*running", r"consum(ing|es)"]
    _LAUNCH_PATTERNS = [r"\bopen\b", r"\blaunch\b", r"\bstart\b"]
    _CLOSE_PATTERNS = [r"\bclose\b", r"\bquit\b", r"\bstop\b"]
    _KILL_PATTERNS = [r"\bkill\b", r"\bterminate\b"]

    # Maps free-text app mentions to the broker's allowlisted app_ids. This
    # is a UX convenience mapping only -- the broker independently enforces
    # its own allowlist regardless of what is sent here, so an unmapped or
    # incorrectly mapped name still cannot launch anything unapproved.
    _APP_ALIASES = {
        "firefox": "firefox",
        "vscode": "vscode",
        "vs code": "vscode",
        "code": "vscode",
        "terminal": "terminal",
        "file manager": "file_manager",
        "files": "file_manager",
    }

    # For "slow/performance" queries the agent collects all four signals
    # before synthesizing an answer (CPU, memory, process list, disk).
    _DIAGNOSIS_TOOLS: list[str] = [
        "get_cpu_info",
        "get_memory_info",
        "list_processes",
        "get_disk_info",
    ]

    def decide(self, user_message: str) -> AgentAction:
        text = user_message.lower().strip()

        sensitive = self._match_sensitive_action(text)
        if sensitive is not None:
            return sensitive

        launch_match = self._match_launch(text)
        if launch_match is not None:
            return AgentAction(kind="call_tool", tool="launch_application", arguments={"app_id": launch_match})

        if any(re.search(p, text) for p in self._PROCESS_PATTERNS):
            return AgentAction(kind="call_tool", tool="list_processes")

        if any(re.search(p, text) for p in self._PERFORMANCE_PATTERNS):
            # A general "how's my computer doing" needs more than one
            # signal; Phase 1 keeps the model to one tool call per turn, so
            # this starts with memory (usually the most actionable single
            # signal for "why is it slow") -- see architecture.md's
            # "Multi-tool turns" section for why chaining is deferred.
            if "disk" in text or "space" in text or "storage" in text:
                return AgentAction(kind="call_tool", tool="get_disk_info")
            if "cpu" in text or "processor" in text:
                return AgentAction(kind="call_tool", tool="get_cpu_info")
            return AgentAction(kind="call_tool", tool="get_memory_info")

        if "system" in text or "hostname" in text or "kernel" in text or "uptime" in text:
            return AgentAction(kind="call_tool", tool="get_system_info")

        return AgentAction(
            kind="respond",
            text=(
                "I can check your system info, CPU, memory, disk, running processes, "
                "or open an approved application (Firefox, VS Code, Terminal, File Manager). "
                "What would you like to know?"
            ),
        )

    def decide_next(
        self,
        user_message: str,
        accumulated: "list[ToolResultEntry]",
        context: "ConversationContext",
    ) -> AgentAction:
        """Multi-step decision for the agent loop.

        For performance/diagnosis queries, collects all four signals before
        synthesizing.  For all other queries, falls back to single-tool
        `decide` on the first iteration.
        """
        text = user_message.lower().strip()
        is_diagnosis = bool(
            re.search(r"\bslow\b", text)
            or re.search(r"\bperformance\b", text)
            or re.search(r"why.*computer", text)
            or re.search(r"what.*using.*most", text)
            or re.search(r"what.*slow", text)
        )

        called = {e.tool for e in accumulated}

        sensitive = self._match_sensitive_action(text, context=context)
        if sensitive is not None:
            return sensitive

        if is_diagnosis:
            # Return the next uncalled diagnosis tool, or respond if all done.
            for tool in self._DIAGNOSIS_TOOLS:
                if tool not in called:
                    return AgentAction(kind="call_tool", tool=tool)
            # All diagnosis tools collected -- signal time to synthesize.
            return AgentAction(kind="respond", text="")

        # Non-diagnosis: single-tool then respond.
        if not accumulated:
            return self.decide(user_message)
        # Already have a result -- synthesize now.
        return AgentAction(kind="respond", text="")

    def synthesize(
        self,
        user_message: str,
        accumulated: "list[ToolResultEntry]",
        context: "ConversationContext",
    ) -> str:
        """Combine multiple tool results into one coherent answer."""
        if not accumulated:
            return "I don't have enough information to answer that."

        parts: list[str] = []
        for entry in accumulated:
            if entry.ok:
                try:
                    parts.append(self.explain(user_message, entry.tool, entry.result))
                except Exception:
                    pass

        if not parts:
            return "I wasn't able to retrieve any system information."

        text = user_message.lower()
        if (
            re.search(r"\bslow\b", text)
            or re.search(r"\bperformance\b", text)
            or re.search(r"why.*computer", text)
        ):
            diagnosis = self._diagnose(accumulated)
            return "\n".join(parts) + (f"\n\n{diagnosis}" if diagnosis else "")

        return "\n".join(parts)

    def _match_launch(self, text: str) -> str | None:
        if not any(re.search(p, text) for p in self._LAUNCH_PATTERNS):
            return None
        return self._match_app_alias(text)

    def _match_app_alias(self, text: str) -> str | None:
        for alias, app_id in self._APP_ALIASES.items():
            if alias in text:
                return app_id
        return None

    def _match_sensitive_action(
        self,
        text: str,
        *,
        context: "ConversationContext | None" = None,
    ) -> AgentAction | None:
        pid_match = re.search(r"\b(?:pid|process)\s+(\d+)\b", text)
        if pid_match and any(re.search(p, text) for p in self._KILL_PATTERNS + self._CLOSE_PATTERNS):
            return AgentAction(
                kind="call_tool",
                tool="kill_process",
                arguments={"pid": int(pid_match.group(1))},
            )

        app_id = self._match_app_alias(text)
        if app_id is not None and any(re.search(p, text) for p in self._CLOSE_PATTERNS):
            return AgentAction(
                kind="call_tool",
                tool="close_application",
                arguments={"app_id": app_id},
            )

        if context is None or not any(
            re.search(p, text) for p in self._CLOSE_PATTERNS + self._KILL_PATTERNS
        ):
            return None

        entity = context.last_entity or ""
        if entity:
            resolved_app = self._match_app_alias(entity)
            if resolved_app is not None and any(re.search(p, text) for p in self._CLOSE_PATTERNS):
                return AgentAction(
                    kind="call_tool",
                    tool="close_application",
                    arguments={"app_id": resolved_app},
                )
            process = context.find_process(entity)
            if process is not None and "pid" in process:
                return AgentAction(
                    kind="call_tool",
                    tool="kill_process",
                    arguments={"pid": int(process["pid"])},
                )

        top_process = context.top_process()
        if top_process is not None and "pid" in top_process:
            return AgentAction(
                kind="call_tool",
                tool="kill_process",
                arguments={"pid": int(top_process["pid"])},
            )
        return None

    def explain(self, user_message: str, tool: str, result: dict[str, Any]) -> str:
        if tool == "get_memory_info":
            top = result.get("top_consumers") or []
            top_line = ""
            if top:
                first = top[0]
                gb = first["bytes"] / (1024 ** 3)
                top_line = f" The biggest consumer is {first['name']} at {gb:.1f} GB."
            return f"Memory usage is at {result['used_percent']:.1f}%.{top_line}"

        if tool == "get_cpu_info":
            return f"CPU usage is currently {result['usage_percent']:.1f}% across {result['core_count']} cores."

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

        if tool == "close_application":
            return f"Closed {result['app_id']} (pid {result['pid']})."

        if tool == "kill_process":
            return f"Terminated process {result['name']} (pid {result['pid']})."

        return f"Done: {result}"

    def explain_error(self, user_message: str, tool: str, error_code: str, error_message: str) -> str:
        if error_code == "not_allowlisted":
            return (
                "I can only open a small set of approved applications "
                "(Firefox, VS Code, Terminal, File Manager) — that app isn't one of them."
            )
        if error_code == "invalid_arguments":
            return "I wasn't able to form a valid request for that — could you rephrase?"
        if error_code == "internal_error":
            return "Something went wrong talking to the system tools. Please try again in a moment."
        return f"I couldn't complete that: {error_message}"

    # ------------------------------------------------------------------
    # Diagnosis helpers
    # ------------------------------------------------------------------

    @staticmethod
    def _diagnose(accumulated: "list[ToolResultEntry]") -> str:
        """Generate a brief conclusion from accumulated diagnosis results."""
        cpu_pct: float | None = None
        mem_pct: float | None = None
        disk_pct: float | None = None
        top_proc: str | None = None

        for entry in accumulated:
            if not entry.ok:
                continue
            r = entry.result
            if entry.tool == "get_cpu_info":
                cpu_pct = float(r.get("usage_percent") or 0)
            elif entry.tool == "get_memory_info":
                mem_pct = float(r.get("used_percent") or 0)
            elif entry.tool == "get_disk_info":
                vols = r.get("volumes") or []
                if vols:
                    disk_pct = max(float(v.get("used_percent") or 0) for v in vols)
            elif entry.tool == "list_processes":
                procs = r.get("processes") or []
                if procs:
                    top = sorted(procs, key=lambda p: p.get("cpu_percent", 0), reverse=True)
                    top_proc = top[0].get("name") if top else None

        causes: list[str] = []
        if cpu_pct is not None and cpu_pct >= 70:
            causes.append(f"high CPU usage ({cpu_pct:.0f}%)")
        if mem_pct is not None and mem_pct >= 80:
            causes.append(f"high memory usage ({mem_pct:.0f}%)")
        if disk_pct is not None and disk_pct >= 90:
            causes.append(f"disk nearly full ({disk_pct:.0f}%)")

        if not causes:
            cause_str = "no single dominant cause was found — the system appears healthy overall"
        else:
            cause_str = " and ".join(causes)

        conclusion = f"Likely cause: {cause_str}."
        if top_proc and causes:
            conclusion += f" The process '{top_proc}' is the top CPU consumer."
        return conclusion
