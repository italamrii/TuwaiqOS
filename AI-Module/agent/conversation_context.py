"""Phase 4: Conversation Context for the Tuwaiq AI Agent.

Maintains a bounded rolling window of conversation turns, relevant tool
results, and entity references (so "it", "this", "open it", "close it" can
be resolved to the last-mentioned process or application).

Design constraints:
- Never stores raw system telemetry dumps in every prompt -- only compact
  summaries suitable for a short context injection.
- Bounded window (MAX_TURNS) prevents unbounded memory growth.
- No shell access, no OS calls; this is a pure in-memory state object.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import Any


@dataclass
class ToolResultEntry:
    """A single tool call result stored in conversation context."""

    tool: str
    result: dict[str, Any]
    ok: bool
    # Compact plain-text summary used in prompt injection (not raw data).
    summary: str = ""


@dataclass
class PendingConfirmation:
    """A specific sensitive action awaiting explicit user approval."""

    tool: str
    arguments: dict[str, Any]
    description: str


@dataclass
class ConversationTurn:
    """One turn in the conversation history."""

    role: str  # "user" | "assistant"
    content: str
    tool_results: list[ToolResultEntry] = field(default_factory=list)


# Pronouns and demonstratives the agent should try to resolve.
_REFERENCE_PATTERNS: list[re.Pattern[str]] = [
    re.compile(r"\b(it|that|this|them)\b", re.IGNORECASE),
    re.compile(r"\bopen\s+(it|that|this)\b", re.IGNORECASE),
    re.compile(r"\bclose\s+(it|that|this)\b", re.IGNORECASE),
    re.compile(r"\bkill\s+(it|that|this)\b", re.IGNORECASE),
]


class ConversationContext:
    """Bounded conversation context for the agent loop.

    Tracks:
    - Recent conversation turns (user + assistant), bounded to MAX_TURNS.
    - The last named entity (process/app) for pronoun resolution.
    - Accumulated tool results from the most recent assistant turn.
    """

    MAX_TURNS: int = 10  # rolling window; older turns are dropped

    def __init__(self) -> None:
        self._turns: list[ConversationTurn] = []
        # Last explicitly named entity (app/process name).
        self.last_entity: str | None = None
        # Most recent process list for deterministic "close it"/"kill it"
        # follow-ups that need a pid, not just a display name.
        self.last_process_list: list[dict[str, Any]] = []
        # Tool results accumulated during the most recent turn (for follow-ups).
        self.last_tool_results: list[ToolResultEntry] = []
        # Sensitive tool request pending explicit yes/no approval.
        self.pending_confirmation: PendingConfirmation | None = None

    # ------------------------------------------------------------------
    # Mutation
    # ------------------------------------------------------------------

    def add_user_turn(self, message: str) -> None:
        """Record a user message."""
        self._turns.append(ConversationTurn(role="user", content=message))
        self._trim()

    def add_assistant_turn(
        self,
        response: str,
        tool_results: list[ToolResultEntry] | None = None,
    ) -> None:
        """Record an assistant response and its associated tool results."""
        results = tool_results or []
        self._turns.append(
            ConversationTurn(role="assistant", content=response, tool_results=results)
        )
        self._trim()
        if results:
            self.last_tool_results = list(results)

    def update_entity(self, entity: str) -> None:
        """Record the last explicitly named app or process."""
        name = entity.strip().lower()
        if name:
            self.last_entity = name

    def update_entity_from_tool_result(self, tool: str, result: dict[str, Any]) -> None:
        """Extract and cache the most prominent entity from a tool result."""
        if tool == "list_processes":
            processes = result.get("processes") or []
            self.last_process_list = list(processes)
            if processes:
                top = sorted(
                    processes, key=lambda p: p.get("cpu_percent", 0.0), reverse=True
                )
                name = top[0].get("name", "") if top else ""
                if name:
                    self.update_entity(name)
        elif tool == "get_memory_info":
            consumers = result.get("top_consumers") or []
            if consumers:
                name = consumers[0].get("name", "")
                if name:
                    self.update_entity(name)
        elif tool == "launch_application":
            app_id = result.get("app_id") or ""
            if app_id:
                self.update_entity(app_id)

    def set_pending_confirmation(
        self, tool: str, arguments: dict[str, Any], description: str
    ) -> None:
        self.pending_confirmation = PendingConfirmation(
            tool=tool,
            arguments=dict(arguments),
            description=description,
        )

    def clear_pending_confirmation(self) -> None:
        self.pending_confirmation = None

    def top_process(self) -> dict[str, Any] | None:
        if self.last_process_list:
            return dict(self.last_process_list[0])
        return None

    def find_process(self, name: str) -> dict[str, Any] | None:
        needle = name.strip().lower()
        if not needle:
            return None
        for process in self.last_process_list:
            process_name = str(process.get("name", "")).strip().lower()
            if process_name == needle:
                return dict(process)
        return None

    # ------------------------------------------------------------------
    # Reference resolution
    # ------------------------------------------------------------------

    def has_references(self, text: str) -> bool:
        """Return True if *text* contains an ambiguous pronoun or reference."""
        return any(p.search(text) for p in _REFERENCE_PATTERNS)

    def resolve_references(self, text: str) -> str:
        """Replace ambiguous references in *text* with the last known entity.

        Only substitutes when a known entity exists.  Returns *text* unchanged
        when there is nothing to resolve.
        """
        if not self.last_entity:
            return text
        entity = self.last_entity
        out = text
        # Compound verb phrases first (more specific).
        out = re.sub(
            r"\bopen\s+(it|that|this)\b", f"open {entity}", out, flags=re.IGNORECASE
        )
        out = re.sub(
            r"\bclose\s+(it|that|this)\b", f"close {entity}", out, flags=re.IGNORECASE
        )
        out = re.sub(
            r"\bkill\s+(it|that|this)\b", f"kill {entity}", out, flags=re.IGNORECASE
        )
        # Bare pronouns last.
        out = re.sub(r"\b(it|that|this)\b", entity, out, flags=re.IGNORECASE)
        return out

    # ------------------------------------------------------------------
    # Prompt context building
    # ------------------------------------------------------------------

    def build_context_prefix(self) -> str:
        """Build a compact, bounded context string for prompt injection.

        Includes only the most recent turns and their summaries -- never raw
        telemetry.  Returns an empty string when there is nothing useful.
        """
        if not self._turns:
            return ""

        lines: list[str] = ["[Conversation context]"]
        # Include at most the last 4 turns to keep prompts short.
        for turn in self._turns[-4:]:
            prefix = "User" if turn.role == "user" else "Assistant"
            lines.append(f"{prefix}: {turn.content[:200]}")
            for tr in turn.tool_results:
                if tr.summary:
                    lines.append(f"  [Tool {tr.tool}]: {tr.summary[:150]}")

        if self.last_entity:
            lines.append(f"[Last mentioned entity: {self.last_entity}]")

        return "\n".join(lines)

    # ------------------------------------------------------------------
    # Read-only access
    # ------------------------------------------------------------------

    @property
    def turns(self) -> list[ConversationTurn]:
        return list(self._turns)

    # ------------------------------------------------------------------
    # Internal
    # ------------------------------------------------------------------

    def _trim(self) -> None:
        if len(self._turns) > self.MAX_TURNS:
            self._turns = self._turns[-self.MAX_TURNS :]
