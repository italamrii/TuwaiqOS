"""Conversation context.

Two distinct kinds of "memory" this class holds, deliberately kept
separate:

1. `turns` -- a plain transcript of completed (user, assistant) exchanges,
   for the model to see prior conversational flow.
2. Structured, typed memory (`last_process_list`, `pending_confirmation`)
   -- specific facts the agent itself tracks deterministically, not left to
   the model to "remember" from free text. This is what makes "what's the
   top program?" -> "close it" reliable: the pid the user means is resolved
   by `Agent`/`RuleBasedProvider` reading `last_process_list`, not by
   re-parsing prior chat text and hoping the model recalls it correctly.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class Turn:
    user_message: str
    assistant_response: str


@dataclass
class ToolCallRecord:
    """One tool call made during the *current* turn's reasoning steps --
    reset at the start of every new user turn (see `Conversation.start_turn`).
    Lets a stepwise `ModelProvider` see "I already checked memory this turn,
    now what" without re-querying the broker."""

    tool: str
    arguments: dict[str, Any]
    ok: bool
    result: dict[str, Any] | None = None
    error_message: str | None = None


@dataclass
class PendingConfirmation:
    """A sensitive tool call the agent has described to the user but not
    yet executed, awaiting an explicit yes/no on the *next* turn. See
    `agent.py`'s `SENSITIVE_TOOLS` gate -- this object is how that gate's
    state survives between one `Agent.handle()` call and the next."""

    tool: str
    arguments: dict[str, Any]
    description: str


@dataclass
class Conversation:
    turns: list[Turn] = field(default_factory=list)
    last_process_list: list[dict[str, Any]] | None = None
    pending_confirmation: PendingConfirmation | None = None
    current_turn_steps: list[ToolCallRecord] = field(default_factory=list)

    def start_turn(self) -> None:
        """Call once at the beginning of handling a new user message --
        clears this-turn tool-call scratch space (not the cross-turn
        memory like `last_process_list`, which persists deliberately)."""
        self.current_turn_steps = []

    def record_tool_call(self, record: ToolCallRecord) -> None:
        self.current_turn_steps.append(record)
        if record.tool == "list_processes" and record.ok and record.result:
            self.last_process_list = record.result.get("processes")

    def finish_turn(self, user_message: str, assistant_response: str) -> None:
        self.turns.append(Turn(user_message, assistant_response))

    def top_process(self) -> dict[str, Any] | None:
        """The process a bare 'it'/'that'/'the top one' most likely refers
        to -- the first entry of the last `list_processes` result, which
        the broker already returns sorted by CPU usage descending."""
        if self.last_process_list:
            return self.last_process_list[0]
        return None
