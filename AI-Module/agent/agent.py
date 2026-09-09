"""Agent core: the orchestration layer between a ModelProvider and the
BrokerClient.

Security-relevant invariant this file exists to enforce: **this file never
calls subprocess, os.system, os.exec*, or anything else that touches the OS
directly.** The only way anything in this process can affect the outside
world is `self._broker.call(tool, arguments)`, and `tool` is checked against
`KNOWN_TOOLS` before that call is ever made -- so even if a future, real
model provider hallucinated an arbitrary tool name or a shell-command-shaped
string, it is rejected here, in Python, before it would even reach the
broker (which independently re-validates it again on the Rust side -- two
layers, not one, is deliberate; see architecture.md's "Defense in depth").

Phase 4 adds a bounded multi-step agent loop (`handle_with_context`) on top
of the single-turn foundation.  `handle` (Phase 1–3 public API) is preserved
as a compatibility wrapper that creates a fresh ephemeral context.
"""

from __future__ import annotations

import logging
from typing import Any

from broker_client import BrokerClient, BrokerUnavailableError
from conversation_context import ConversationContext, PendingConfirmation, ToolResultEntry
from model_provider import AgentAction, ModelProvider
from protocol import KNOWN_TOOLS

logger = logging.getLogger("tuwaiq_agent.agent")

# Maximum number of tool calls the agent loop will make for a single user
# request.  Prevents runaway loops; 5 is enough for a full system diagnosis
# (cpu + memory + processes + disk + network if relevant).
MAX_LOOP_ITERATIONS: int = 5
SENSITIVE_TOOLS = frozenset({"close_application", "kill_process"})
CONFIRM_YES = frozenset({"yes", "y", "allow", "approve", "confirm"})
CONFIRM_NO = frozenset({"no", "n", "deny", "reject", "cancel"})


class Agent:
    def __init__(self, model: ModelProvider, broker: BrokerClient):
        self._model = model
        self._broker = broker

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def handle(self, user_message: str) -> str:
        """Single-turn handler (Phase 1–3 compatibility).

        Creates an ephemeral context for the turn so existing callers and
        tests need not change.  For multi-turn sessions use
        `handle_with_context` directly.
        """
        return self.handle_with_context(user_message, ConversationContext())

    def handle_with_context(
        self, user_message: str, context: ConversationContext
    ) -> str:
        """Multi-step agent loop with bounded iterations.

        Flow per user request:
          1. Resolve any pronouns/references ("it", "open it") from context.
          2. Ask the model what to do next (call a tool or respond).
          3. If respond → return immediately.
          4. If call_tool → validate, call broker, update context.
          5. Repeat from step 2 up to MAX_LOOP_ITERATIONS times.
          6. After the loop, synthesize a final answer from accumulated results.

        Safeguards:
        - Repeated tool calls are detected and stop the loop.
        - Unknown tools are rejected before reaching the broker.
        - Model failures are caught and do not crash the agent.
        - Broker unavailability surfaces a safe user-facing message.
        """
        context.add_user_turn(user_message)
        pending = context.pending_confirmation
        if pending is not None:
            return self._handle_confirmation_reply(user_message, context, pending)

        resolved = context.resolve_references(user_message)

        accumulated: list[ToolResultEntry] = []
        called_tools: set[str] = set()

        for iteration in range(MAX_LOOP_ITERATIONS):
            try:
                action = self._model.decide_next(resolved, accumulated, context)
            except Exception:
                logger.exception(
                    "model provider raised at loop iteration %d; stopping loop", iteration
                )
                break

            if action.kind == "respond":
                text = action.text or ""
                if text:
                    # Model provided a complete answer — return it directly.
                    context.add_assistant_turn(text, accumulated)
                    return text
                # Empty text is a synthesize-now signal from the provider.
                # Break out of the loop so the synthesis path runs below.
                break

            tool = action.tool or ""

            # Reject unknown tools before the broker ever sees the request.
            if tool not in KNOWN_TOOLS:
                logger.warning(
                    "model requested unknown tool %r at iteration %d; stopping loop",
                    tool,
                    iteration,
                )
                break

            # Prevent repeated calls to the same tool in one request.
            if tool in called_tools:
                logger.warning(
                    "repeated tool call to %r at iteration %d; stopping loop",
                    tool,
                    iteration,
                )
                break

            called_tools.add(tool)

            if tool in SENSITIVE_TOOLS:
                description = self._describe_sensitive_action(tool, action.arguments, context)
                context.set_pending_confirmation(tool, action.arguments, description)
                text = (
                    f"{description} This requires your explicit confirmation. "
                    "Reply with yes/allow/approve to continue or no/deny to cancel."
                )
                context.add_assistant_turn(text, accumulated)
                return text

            try:
                response = self._broker.call(tool, action.arguments)
            except BrokerUnavailableError as exc:
                logger.error("broker unavailable: %s", exc)
                text = "System tools are temporarily unavailable. Please try again shortly."
                context.add_assistant_turn(text, accumulated)
                return text

            entry = ToolResultEntry(
                tool=tool,
                result=response.result or {},
                ok=response.ok,
                summary=self._summarize_result(tool, response.result, response.ok),
            )
            accumulated.append(entry)
            context.update_entity_from_tool_result(tool, response.result or {})

        # Loop finished (max iterations, repeated tool, or break).
        # Synthesize a final answer from whatever we collected.
        if accumulated:
            try:
                text = self._model.synthesize(resolved, accumulated, context)
                context.add_assistant_turn(text, accumulated)
                return text
            except Exception:
                logger.exception("model provider raised during synthesize")
                # Fall through to plain concatenation below.
                text = self._fallback_synthesize(resolved, accumulated)
                context.add_assistant_turn(text, accumulated)
                return text

        text = "I wasn't able to gather enough information to answer that."
        context.add_assistant_turn(text, [])
        return text

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    @staticmethod
    def _summarize_result(
        tool: str, result: dict[str, Any] | None, ok: bool
    ) -> str:
        """Build a compact one-line summary of a tool result for context injection."""
        if not ok or result is None:
            return f"{tool}: error"
        if tool == "get_cpu_info":
            return f"CPU {result.get('usage_percent', '?')}% across {result.get('core_count', '?')} cores"
        if tool == "get_memory_info":
            pct = result.get("used_percent", "?")
            top = (result.get("top_consumers") or [{}])[0].get("name", "")
            return f"RAM {pct}% used" + (f", top: {top}" if top else "")
        if tool == "get_disk_info":
            vols = result.get("volumes") or []
            parts = [f"{v.get('mount_point','?')} {v.get('used_percent','?')}%" for v in vols[:2]]
            return "Disk: " + ", ".join(parts) if parts else "Disk: no volumes"
        if tool == "list_processes":
            procs = result.get("processes") or []
            if procs:
                top = sorted(procs, key=lambda p: p.get("cpu_percent", 0), reverse=True)
                return f"Top process: {top[0].get('name','?')} ({top[0].get('cpu_percent','?')}% CPU)"
            return "processes: none"
        if tool == "get_network_status":
            ifaces = result.get("interfaces") or []
            names = [i.get("name", "?") for i in ifaces[:2]]
            return "Network: " + ", ".join(names) if names else "Network: no interfaces"
        if tool == "get_system_info":
            return f"OS: {result.get('os_name','?')} {result.get('os_version','?')}"
        if tool == "launch_application":
            return f"Launched {result.get('app_id','?')} (pid {result.get('pid','?')})"
        return f"{tool}: ok"

    def _fallback_synthesize(
        self, user_message: str, accumulated: list[ToolResultEntry]
    ) -> str:
        """Plain fallback when the model synthesize call fails."""
        parts: list[str] = []
        for entry in accumulated:
            if entry.ok and entry.summary:
                parts.append(entry.summary)
            elif not entry.ok:
                parts.append(f"{entry.tool}: error")
        if parts:
            return "Here is what I found: " + "; ".join(parts) + "."
        return "I couldn't retrieve the requested information."

    def _handle_confirmation_reply(
        self,
        user_message: str,
        context: ConversationContext,
        pending: PendingConfirmation,
    ) -> str:
        normalized = user_message.strip().lower()
        if normalized in CONFIRM_NO:
            context.clear_pending_confirmation()
            text = f"Okay — I will not proceed. Cancelled: {pending.description}"
            context.add_assistant_turn(text, [])
            return text

        if normalized not in CONFIRM_YES:
            text = (
                f"I still need an explicit yes/allow/approve or no/deny for this action: "
                f"{pending.description}"
            )
            context.add_assistant_turn(text, [])
            return text

        context.clear_pending_confirmation()
        try:
            response = self._broker.call(pending.tool, pending.arguments)
        except BrokerUnavailableError as exc:
            logger.error("broker unavailable during confirmed action: %s", exc)
            text = "System tools are temporarily unavailable, so I couldn't complete the confirmed action."
            context.add_assistant_turn(text, [])
            return text

        entry = ToolResultEntry(
            tool=pending.tool,
            result=response.result or {},
            ok=response.ok,
            summary=self._summarize_result(pending.tool, response.result, response.ok),
        )
        if response.ok:
            context.update_entity_from_tool_result(pending.tool, response.result or {})
            text = self._confirmed_success_text(pending, response.result or {})
            context.add_assistant_turn(text, [entry])
            return text

        error_message = response.error_message or "the broker rejected the action"
        text = f"I couldn't complete the confirmed action: {error_message}"
        context.add_assistant_turn(text, [entry])
        return text

    @staticmethod
    def _describe_sensitive_action(
        tool: str,
        arguments: dict[str, Any],
        context: ConversationContext,
    ) -> str:
        if tool == "close_application":
            app_id = str(arguments.get("app_id", "that application"))
            return f"Tuwaiq AI wants to close {app_id}."
        if tool == "kill_process":
            pid = arguments.get("pid", "?")
            process = context.find_process(str(context.last_entity or "")) or context.top_process() or {}
            name = process.get("name")
            if name:
                return f"Tuwaiq AI wants to terminate {name} (pid {pid})."
            return f"Tuwaiq AI wants to terminate process pid {pid}."
        return f"Tuwaiq AI wants to execute {tool}."

    @staticmethod
    def _confirmed_success_text(
        pending: PendingConfirmation,
        result: dict[str, Any],
    ) -> str:
        if pending.tool == "close_application":
            return f"Confirmed — closed {result.get('app_id', 'the application')} (pid {result.get('pid', '?')})."
        if pending.tool == "kill_process":
            return f"Confirmed — terminated {result.get('name', 'the process')} (pid {result.get('pid', '?')})."
        return f"Confirmed — completed {pending.tool}."
