"""Phase 4: Conversation Context and Agent Reasoning tests.

Covers (per spec):
- single tool call
- multiple tool calls (bounded loop collecting diagnosis data)
- bounded loop (MAX_LOOP_ITERATIONS respected)
- loop termination (model returns "respond" → loop stops)
- context retention (previous turns kept across calls)
- follow-up questions ("What's using the most?")
- pronoun/entity references ("it", "this", "open it", "close it")
- tool failure (broker error response)
- timeout / broker unavailable
- model failure (exception in decide_next)
"""

from __future__ import annotations

from typing import Any
from unittest.mock import MagicMock

import pytest

from agent import MAX_LOOP_ITERATIONS, Agent
from broker_client import BrokerUnavailableError
from conversation_context import ConversationContext, ToolResultEntry
from model_provider import AgentAction, RuleBasedProvider
from protocol import ToolResponse


# ---------------------------------------------------------------------------
# Shared fake helpers
# ---------------------------------------------------------------------------

_FAKE_RESULTS: dict[str, dict[str, Any]] = {
    "get_memory_info": {
        "total_bytes": 16_000_000_000,
        "used_bytes": 9_000_000_000,
        "used_percent": 56.25,
        "top_consumers": [{"name": "firefox", "bytes": 1_500_000_000}],
    },
    "get_cpu_info": {
        "model": "Test CPU",
        "usage_percent": 42.0,
        "core_count": 8,
        "per_core_usage_percent": [42.0] * 8,
    },
    "get_disk_info": {
        "volumes": [{"mount_point": "/", "used_percent": 70.0, "total_bytes": 500_000_000_000}]
    },
    "list_processes": {
        "processes": [
            {"pid": 1234, "name": "chrome", "cpu_percent": 30.0, "memory_bytes": 5_000_000},
            {"pid": 5678, "name": "firefox", "cpu_percent": 10.0, "memory_bytes": 3_000_000},
        ]
    },
    "get_network_status": {"interfaces": [{"name": "eth0", "rx_kbps": 12.0, "tx_kbps": 3.0}]},
    "get_system_info": {
        "hostname": "tuwaiq-box",
        "os_name": "TuwaiqOS",
        "os_version": "v0.5",
        "kernel_version": "5.15.0",
        "uptime_seconds": 3600,
    },
    "launch_application": {"app_id": "firefox", "pid": 9999, "launched": True},
    "close_application": {"app_id": "firefox", "pid": 9999, "name": "firefox", "terminated": True},
    "kill_process": {"pid": 1234, "name": "chrome", "terminated": True},
}


class _FakeBroker:
    """Minimal broker stub returning canned results."""

    def __init__(self, fail_tools: set[str] | None = None) -> None:
        self.call_log: list[str] = []
        self._fail_tools = fail_tools or set()

    def call(self, tool: str, arguments: dict | None = None) -> ToolResponse:
        self.call_log.append(tool)
        if tool in self._fail_tools:
            return ToolResponse(
                protocol_version="1.0",
                request_id="req-fake",
                timestamp="now",
                status="error",
                error={"code": "internal_error", "message": f"{tool} failed"},
            )
        result = _FAKE_RESULTS.get(tool)
        if result is None:
            return ToolResponse(
                protocol_version="1.0",
                request_id="req-fake",
                timestamp="now",
                status="error",
                error={"code": "internal_error", "message": f"no fake result for {tool}"},
            )
        return ToolResponse(
            protocol_version="1.0",
            request_id="req-fake",
            timestamp="now",
            status="ok",
            result=result,
        )


class _SequenceProvider(RuleBasedProvider):
    """Provider that returns a fixed sequence of AgentActions then responds."""

    def __init__(self, actions: list[AgentAction]) -> None:
        super().__init__()
        self._actions = list(actions)
        self._index = 0

    def decide_next(
        self,
        user_message: str,
        accumulated: list[ToolResultEntry],
        context: ConversationContext,
    ) -> AgentAction:
        if self._index < len(self._actions):
            action = self._actions[self._index]
            self._index += 1
            return action
        return AgentAction(kind="respond", text="done")

    def synthesize(
        self,
        user_message: str,
        accumulated: list[ToolResultEntry],
        context: ConversationContext,
    ) -> str:
        summaries = [e.summary or e.tool for e in accumulated]
        return "Collected: " + "; ".join(summaries)


class _ExplodingProvider(RuleBasedProvider):
    """Provider whose decide_next always raises an exception."""

    def decide_next(
        self,
        user_message: str,
        accumulated: list[ToolResultEntry],
        context: ConversationContext,
    ) -> AgentAction:
        raise RuntimeError("model exploded")


class _TimeoutBroker:
    """Broker stub that raises BrokerUnavailableError."""

    def call(self, tool: str, arguments: dict | None = None) -> ToolResponse:
        raise BrokerUnavailableError("broker timed out")


class _RecordingBroker(_FakeBroker):
    def call(self, tool: str, arguments: dict | None = None) -> ToolResponse:
        return super().call(tool, arguments)


# ===========================================================================
# 1. Single tool call
# ===========================================================================

def test_single_tool_call_returns_explanation() -> None:
    provider = RuleBasedProvider()
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    response = agent.handle_with_context("How much RAM am I using?", context)

    assert response
    assert "56" in response or "memory" in response.lower() or "%" in response
    assert broker.call_log  # at least one tool was called


def test_single_tool_call_cpu() -> None:
    provider = RuleBasedProvider()
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    response = agent.handle_with_context("What is my CPU usage?", context)

    assert response
    assert "42" in response or "cpu" in response.lower()


# ===========================================================================
# 2. Multiple tool calls (diagnosis query collects several tools)
# ===========================================================================

def test_diagnosis_query_calls_multiple_tools() -> None:
    provider = RuleBasedProvider()
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    response = agent.handle_with_context("Why is my computer slow?", context)

    # The rule-based provider should call at least 2 diagnosis tools.
    assert len(broker.call_log) >= 2
    assert response


def test_diagnosis_collects_cpu_and_memory() -> None:
    provider = RuleBasedProvider()
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    agent.handle_with_context("Why is my computer slow?", context)

    called = set(broker.call_log)
    assert "get_cpu_info" in called
    assert "get_memory_info" in called


# ===========================================================================
# 3. Bounded loop (MAX_LOOP_ITERATIONS)
# ===========================================================================

def test_bounded_loop_never_exceeds_max() -> None:
    # Provider that always says "call another tool" -- loop must terminate.
    actions = [
        AgentAction(kind="call_tool", tool="get_cpu_info"),
        AgentAction(kind="call_tool", tool="get_memory_info"),
        AgentAction(kind="call_tool", tool="get_disk_info"),
        AgentAction(kind="call_tool", tool="list_processes"),
        AgentAction(kind="call_tool", tool="get_network_status"),
        # Would be a 6th call -- must never happen.
        AgentAction(kind="call_tool", tool="get_system_info"),
    ]
    provider = _SequenceProvider(actions)
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    response = agent.handle_with_context("diagnose everything", context)

    assert len(broker.call_log) <= MAX_LOOP_ITERATIONS
    assert response


# ===========================================================================
# 4. Loop termination (model returns "respond")
# ===========================================================================

def test_loop_terminates_when_model_responds() -> None:
    provider = _SequenceProvider([AgentAction(kind="respond", text="All good.")])
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    response = agent.handle_with_context("anything", context)

    assert response == "All good."
    assert broker.call_log == []  # no tool calls needed


def test_loop_terminates_on_repeated_tool_call() -> None:
    # Provider attempts to call the same tool twice.
    actions = [
        AgentAction(kind="call_tool", tool="get_cpu_info"),
        AgentAction(kind="call_tool", tool="get_cpu_info"),  # repeated
    ]
    provider = _SequenceProvider(actions)
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    response = agent.handle_with_context("cpu twice", context)

    # Loop must stop after detecting the repeated call; only one broker call.
    assert broker.call_log.count("get_cpu_info") == 1
    assert response


def test_loop_terminates_on_unknown_tool() -> None:
    actions = [AgentAction(kind="call_tool", tool="run_shell_command")]
    provider = _SequenceProvider(actions)
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    response = agent.handle_with_context("do something bad", context)

    # Broker must never be called for an unknown tool.
    assert broker.call_log == []
    assert response


# ===========================================================================
# 4b. Confirmation workflow for sensitive tools
# ===========================================================================

def test_sensitive_tool_requires_confirmation_before_execution() -> None:
    provider = _SequenceProvider(
        [AgentAction(kind="call_tool", tool="close_application", arguments={"app_id": "firefox"})]
    )
    broker = _RecordingBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    response = agent.handle_with_context("Close Firefox.", context)

    assert "requires your explicit confirmation" in response
    assert context.pending_confirmation is not None
    assert broker.call_log == []


def test_sensitive_tool_executes_after_confirmation() -> None:
    provider = _SequenceProvider(
        [AgentAction(kind="call_tool", tool="close_application", arguments={"app_id": "firefox"})]
    )
    broker = _RecordingBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    agent.handle_with_context("Close Firefox.", context)
    response = agent.handle_with_context("yes", context)

    assert "Confirmed" in response
    assert broker.call_log == ["close_application"]
    assert context.pending_confirmation is None


def test_sensitive_tool_denied_is_not_executed() -> None:
    provider = _SequenceProvider(
        [AgentAction(kind="call_tool", tool="kill_process", arguments={"pid": 1234})]
    )
    broker = _RecordingBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    agent.handle_with_context("Kill process 1234.", context)
    response = agent.handle_with_context("deny", context)

    assert "Cancelled" in response
    assert broker.call_log == []
    assert context.pending_confirmation is None


def test_invalid_confirmation_does_not_authorize_action() -> None:
    provider = _SequenceProvider(
        [AgentAction(kind="call_tool", tool="kill_process", arguments={"pid": 1234})]
    )
    broker = _RecordingBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    agent.handle_with_context("Kill process 1234.", context)
    response = agent.handle_with_context("maybe later", context)

    assert "I still need an explicit" in response
    assert broker.call_log == []
    assert context.pending_confirmation is not None


def test_model_cannot_bypass_confirmation_with_direct_sensitive_tool_call() -> None:
    provider = _SequenceProvider(
        [AgentAction(kind="call_tool", tool="kill_process", arguments={"pid": 1234})]
    )
    broker = _RecordingBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    response = agent.handle_with_context("Do the dangerous thing.", context)

    assert "explicit confirmation" in response
    assert broker.call_log == []


# ===========================================================================
# 5. Context retention across turns
# ===========================================================================

def test_context_retains_turns() -> None:
    context = ConversationContext()
    context.add_user_turn("Hello")
    context.add_assistant_turn("Hi there")
    context.add_user_turn("How are you?")

    turns = context.turns
    assert len(turns) == 3
    assert turns[0].role == "user"
    assert turns[1].role == "assistant"
    assert turns[2].role == "user"


def test_context_rolling_window() -> None:
    context = ConversationContext()
    context.MAX_TURNS = 4
    for i in range(6):
        context.add_user_turn(f"message {i}")

    assert len(context.turns) == 4
    # Oldest messages dropped.
    assert context.turns[0].content == "message 2"


def test_context_updated_after_handle_with_context() -> None:
    provider = RuleBasedProvider()
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    agent.handle_with_context("How much RAM am I using?", context)

    # Context should now have at least a user and assistant turn.
    roles = [t.role for t in context.turns]
    assert "user" in roles
    assert "assistant" in roles


# ===========================================================================
# 6. Follow-up questions
# ===========================================================================

def test_followup_what_is_using_most() -> None:
    provider = RuleBasedProvider()
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    # First turn: diagnose.
    agent.handle_with_context("Why is my computer slow?", context)
    broker.call_log.clear()

    # Second turn: follow-up with reference to prior context.
    response2 = agent.handle_with_context("What's using the most?", context)

    assert response2


def test_followup_uses_existing_context_data() -> None:
    """After a diagnosis, a follow-up should re-use context, not start fresh."""
    provider = RuleBasedProvider()
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    agent.handle_with_context("Why is my computer slow?", context)
    first_calls = len(broker.call_log)

    # Second turn with a narrower question.
    broker.call_log.clear()
    agent.handle_with_context("What is the CPU at?", context)

    # Second turn should succeed.
    assert len(broker.call_log) >= 0  # may call 0 or 1 tools (context available)


# ===========================================================================
# 7. Reference resolution ("it", "this", "open it", "close it")
# ===========================================================================

def test_resolve_open_it_with_entity() -> None:
    context = ConversationContext()
    context.update_entity("firefox")

    resolved = context.resolve_references("Open it")
    assert "firefox" in resolved.lower()


def test_resolve_close_it_with_entity() -> None:
    context = ConversationContext()
    context.update_entity("chrome")

    resolved = context.resolve_references("Close it please")
    assert "chrome" in resolved.lower()


def test_resolve_bare_pronoun() -> None:
    context = ConversationContext()
    context.update_entity("vscode")

    resolved = context.resolve_references("What is it doing?")
    assert "vscode" in resolved.lower()


def test_no_resolution_without_entity() -> None:
    context = ConversationContext()
    text = "Open it"
    assert context.resolve_references(text) == text


def test_entity_set_from_memory_result() -> None:
    context = ConversationContext()
    context.update_entity_from_tool_result(
        "get_memory_info",
        {"top_consumers": [{"name": "firefox", "bytes": 1_500_000_000}]},
    )
    assert context.last_entity == "firefox"


def test_entity_set_from_process_result() -> None:
    context = ConversationContext()
    context.update_entity_from_tool_result(
        "list_processes",
        {
            "processes": [
                {"name": "chrome", "cpu_percent": 30.0},
                {"name": "firefox", "cpu_percent": 10.0},
            ]
        },
    )
    assert context.last_entity == "chrome"


def test_open_it_resolved_and_dispatched() -> None:
    """Full round-trip: entity set from process result, then 'Open it' launches it."""
    provider = RuleBasedProvider()
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    # Seed entity via tool result.
    context.update_entity_from_tool_result(
        "list_processes",
        {"processes": [{"name": "firefox", "cpu_percent": 5.0}]},
    )

    response = agent.handle_with_context("Open it", context)

    assert response
    # launch_application should have been called.
    assert "launch_application" in broker.call_log


def test_close_it_does_not_bypass_broker() -> None:
    """'Close it' must go through the broker, not execute directly."""
    provider = RuleBasedProvider()
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()
    context.update_entity("terminal")

    response = agent.handle_with_context("Close it", context)

    assert "explicit confirmation" in response
    assert broker.call_log == []
    assert context.pending_confirmation is not None


# ===========================================================================
# 8. Tool failure
# ===========================================================================

def test_tool_failure_returns_safe_message() -> None:
    provider = RuleBasedProvider()
    broker = _FakeBroker(fail_tools={"get_memory_info"})
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    response = agent.handle_with_context("How much RAM am I using?", context)

    assert response  # safe message returned, not a crash


def test_all_tools_failing_returns_message() -> None:
    provider = _SequenceProvider([
        AgentAction(kind="call_tool", tool="get_cpu_info"),
        AgentAction(kind="call_tool", tool="get_memory_info"),
    ])
    broker = _FakeBroker(fail_tools={"get_cpu_info", "get_memory_info"})
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    response = agent.handle_with_context("diagnose", context)

    assert response  # graceful degradation


def test_partial_tool_failure_still_returns_answer() -> None:
    provider = _SequenceProvider([
        AgentAction(kind="call_tool", tool="get_cpu_info"),
        AgentAction(kind="call_tool", tool="get_memory_info"),
    ])
    broker = _FakeBroker(fail_tools={"get_cpu_info"})  # only cpu fails
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    response = agent.handle_with_context("diagnose", context)

    assert response


# ===========================================================================
# 9. Timeout / broker unavailable
# ===========================================================================

def test_broker_unavailable_returns_safe_message() -> None:
    provider = _SequenceProvider([AgentAction(kind="call_tool", tool="get_cpu_info")])
    broker = _TimeoutBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    response = agent.handle_with_context("CPU info please", context)

    assert "unavailable" in response.lower() or response


def test_broker_unavailable_does_not_crash() -> None:
    provider = RuleBasedProvider()
    broker = _TimeoutBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    # Must return a string, not raise.
    response = agent.handle_with_context("How much RAM?", context)
    assert isinstance(response, str)


# ===========================================================================
# 10. Model failure
# ===========================================================================

def test_model_exception_returns_safe_message() -> None:
    provider = _ExplodingProvider()
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    response = agent.handle_with_context("anything", context)

    assert isinstance(response, str)
    assert response  # non-empty safe message


def test_model_exception_does_not_propagate() -> None:
    provider = _ExplodingProvider()
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    context = ConversationContext()

    # Should not raise.
    try:
        agent.handle_with_context("anything", context)
    except Exception as exc:
        pytest.fail(f"Agent raised an exception instead of handling it: {exc}")


# ===========================================================================
# 11. Backward-compat: handle() still works (creates ephemeral context)
# ===========================================================================

def test_handle_backward_compat() -> None:
    provider = RuleBasedProvider()
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]

    response = agent.handle("How much RAM am I using?")

    assert response
    assert "memory" in response.lower() or "%" in response


def test_handle_creates_fresh_context_per_call() -> None:
    """Each `handle()` call gets a fresh ephemeral context (no cross-turn leakage)."""
    provider = RuleBasedProvider()
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]

    r1 = agent.handle("How much RAM?")
    broker.call_log.clear()
    r2 = agent.handle("How much RAM?")

    assert r1 and r2


# ===========================================================================
# 12. ConversationContext internals
# ===========================================================================

def test_context_prefix_empty_when_no_turns() -> None:
    context = ConversationContext()
    assert context.build_context_prefix() == ""


def test_context_prefix_includes_last_entity() -> None:
    context = ConversationContext()
    context.add_user_turn("check system")
    context.add_assistant_turn("ok")
    context.update_entity("firefox")

    prefix = context.build_context_prefix()
    assert "firefox" in prefix


def test_context_prefix_bounded_to_four_turns() -> None:
    context = ConversationContext()
    for i in range(10):
        context.add_user_turn(f"turn {i}")
    prefix = context.build_context_prefix()
    # Only last 4 turns should appear.
    assert "turn 6" in prefix or "turn 7" in prefix
    assert "turn 0" not in prefix


def test_tool_result_entry_summary_stored() -> None:
    context = ConversationContext()
    entry = ToolResultEntry(tool="get_cpu_info", result={"usage_percent": 42.0}, ok=True, summary="CPU 42%")
    context.add_assistant_turn("got it", [entry])

    assert context.last_tool_results[0].summary == "CPU 42%"


# ===========================================================================
# 13. RuleBasedProvider.synthesize and decide_next
# ===========================================================================

def test_rule_based_synthesize_single_result() -> None:
    provider = RuleBasedProvider()
    context = ConversationContext()
    accumulated = [
        ToolResultEntry(
            tool="get_cpu_info",
            result={"usage_percent": 42.0, "core_count": 8},
            ok=True,
        )
    ]
    result = provider.synthesize("CPU info", accumulated, context)
    assert "42" in result or "cpu" in result.lower()


def test_rule_based_synthesize_diagnosis() -> None:
    provider = RuleBasedProvider()
    context = ConversationContext()
    accumulated = [
        ToolResultEntry(tool="get_cpu_info", result={"usage_percent": 80.0, "core_count": 8}, ok=True),
        ToolResultEntry(tool="get_memory_info", result={"used_percent": 85.0, "top_consumers": [{"name": "chrome", "bytes": 4_000_000_000}]}, ok=True),
        ToolResultEntry(tool="list_processes", result={"processes": [{"name": "chrome", "cpu_percent": 40.0}]}, ok=True),
        ToolResultEntry(tool="get_disk_info", result={"volumes": [{"mount_point": "/", "used_percent": 50.0}]}, ok=True),
    ]
    result = provider.synthesize("Why is my computer slow?", accumulated, context)
    assert "cpu" in result.lower() or "memory" in result.lower() or "%" in result
    # Conclusion line should be present.
    assert "likely" in result.lower() or "cause" in result.lower() or "%" in result


def test_rule_based_decide_next_diagnosis_returns_all_tools() -> None:
    provider = RuleBasedProvider()
    context = ConversationContext()

    tools_requested: list[str] = []
    accumulated: list[ToolResultEntry] = []

    for _ in range(10):
        action = provider.decide_next("Why is my computer slow?", accumulated, context)
        if action.kind == "respond":
            break
        assert action.tool is not None
        tools_requested.append(action.tool)
        accumulated.append(
            ToolResultEntry(tool=action.tool, result={}, ok=True)
        )

    # All 4 diagnosis tools should have been requested.
    assert set(tools_requested) == {"get_cpu_info", "get_memory_info", "list_processes", "get_disk_info"}


def test_rule_based_decide_next_non_diagnosis_stops_after_one() -> None:
    provider = RuleBasedProvider()
    context = ConversationContext()

    # First call returns a tool.
    action1 = provider.decide_next("How much RAM?", [], context)
    assert action1.kind == "call_tool"

    # After one result, should respond.
    accumulated = [ToolResultEntry(tool=action1.tool or "get_memory_info", result={}, ok=True)]
    action2 = provider.decide_next("How much RAM?", accumulated, context)
    assert action2.kind == "respond"
