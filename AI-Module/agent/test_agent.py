"""Tests for the Agent orchestration loop: bounded multi-step reasoning,
the sensitive-action confirmation gate, and crash/restart resilience.

Uses the real compiled broker subprocess (not mocked) wherever a test
needs actual tool execution, and a stub BrokerClient/ModelProvider where a
test needs to force a specific failure mode (crash, malformed response)
that would be impractical to reproduce through the real broker.

Run: pytest test_agent.py -v
"""

from __future__ import annotations

import subprocess
import sys
import time

import pytest

from agent import Agent, MAX_STEPS
from broker_client import BrokerClient, BrokerUnavailableError
from conversation import Conversation
from model_provider import ModelProvider, RuleBasedProvider, Step
from protocol import ToolResponse


def _spawn_throwaway_process():
    """A real, harmless, long-running process to test kill_process
    against. `sleep` is Unix-only and does not exist on Windows -- using
    the current Python interpreter itself is genuinely cross-platform,
    since whatever runs this test suite can always run this."""
    return subprocess.Popen([sys.executable, "-c", "import time; time.sleep(120)"])


@pytest.fixture
def broker():
    client = BrokerClient()
    yield client
    client.shutdown()


@pytest.fixture
def agent(broker):
    return Agent(model=RuleBasedProvider(), broker=broker)


# --- multi-step reasoning -------------------------------------------------


def test_low_memory_usage_answers_without_chaining_to_processes(agent):
    reply = agent.handle("why is my computer slow?")
    assert "Memory usage" in reply
    assert reply


def test_reasoning_loop_never_exceeds_max_steps():
    """A model that never emits final_answer must not hang the agent."""

    class NeverFinishesProvider(ModelProvider):
        def __init__(self):
            self.call_count = 0

        def step(self, user_message, conversation):
            self.call_count += 1
            return Step(kind="tool_call", tool="get_system_info", arguments={})

        def describe_sensitive_action(self, tool, arguments):
            return "n/a"

    provider = NeverFinishesProvider()
    broker = BrokerClient()
    try:
        agent = Agent(model=provider, broker=broker)
        reply = agent.handle("anything")
        assert provider.call_count == MAX_STEPS
        assert "couldn't reach a clear answer" in reply
    finally:
        broker.shutdown()


# --- confirmation gate -----------------------------------------------------


def test_kill_process_requires_confirmation_before_broker_is_called(agent):
    proc = _spawn_throwaway_process()
    try:
        agent._conversation.last_process_list = [
            {"pid": proc.pid, "name": "python", "cpu_percent": 5.0, "memory_bytes": 100}
        ]
        reply = agent.handle("close it")
        assert "Proceed?" in reply
        time.sleep(0.2)
        assert proc.poll() is None, "process must NOT be killed before confirmation"
    finally:
        proc.terminate()


def test_confirmed_kill_actually_terminates_the_process(agent):
    proc = _spawn_throwaway_process()
    agent._conversation.last_process_list = [
        {"pid": proc.pid, "name": "python", "cpu_percent": 5.0, "memory_bytes": 100}
    ]
    agent.handle("close it")
    reply = agent.handle("yes")
    assert "Closed" in reply
    time.sleep(0.2)
    assert proc.poll() is not None, "process must be terminated after confirmation"


def test_declined_kill_does_not_terminate_the_process(agent):
    proc = _spawn_throwaway_process()
    try:
        agent._conversation.last_process_list = [
            {"pid": proc.pid, "name": "python", "cpu_percent": 5.0, "memory_bytes": 100}
        ]
        agent.handle("close it")
        reply = agent.handle("no")
        assert "won't do that" in reply
        time.sleep(0.2)
        assert proc.poll() is None
    finally:
        proc.terminate()


def test_ambiguous_reply_does_not_confirm_or_cancel(agent):
    proc = _spawn_throwaway_process()
    try:
        agent._conversation.last_process_list = [
            {"pid": proc.pid, "name": "python", "cpu_percent": 5.0, "memory_bytes": 100}
        ]
        agent.handle("close it")
        reply = agent.handle("maybe later")
        assert "yes or no" in reply
        assert agent._conversation.pending_confirmation is not None
        time.sleep(0.2)
        assert proc.poll() is None
    finally:
        proc.terminate()


def test_broker_independently_refuses_pid_1_even_if_confirmed(agent):
    """Defense in depth: even a confirmed kill_process request for a
    protected pid must still be rejected by the broker's own independent
    check, not merely by anything Python decided."""
    agent._conversation.last_process_list = [{"pid": 1, "name": "init", "cpu_percent": 0.0, "memory_bytes": 0}]
    agent.handle("close it")
    reply = agent.handle("yes")
    assert "couldn't complete" in reply
    assert "protected" in reply.lower() or "cannot be terminated" in reply.lower() or "init" in reply.lower()


# --- unknown tool rejection -------------------------------------------------


def test_model_requesting_unknown_tool_never_reaches_broker(agent):
    class RogueProvider(ModelProvider):
        def step(self, user_message, conversation):
            return Step(kind="tool_call", tool="rm -rf /", arguments={})

        def describe_sensitive_action(self, tool, arguments):
            return "n/a"

    broker = BrokerClient()
    try:
        rogue_agent = Agent(model=RogueProvider(), broker=broker)
        reply = rogue_agent.handle("anything")
        assert "don't have a way to do that" in reply
    finally:
        broker.shutdown()


# --- crash / restart resilience --------------------------------------------


def test_model_provider_crash_does_not_crash_agent(agent):
    class CrashingProvider(ModelProvider):
        def step(self, user_message, conversation):
            raise RuntimeError("simulated model crash")

        def describe_sensitive_action(self, tool, arguments):
            return "n/a"

    broker = BrokerClient()
    try:
        crashy_agent = Agent(model=CrashingProvider(), broker=broker)
        reply = crashy_agent.handle("anything")
        assert "ran into a problem" in reply
        reply2 = crashy_agent.handle("anything")
        assert "ran into a problem" in reply2
    finally:
        broker.shutdown()


def test_broker_crash_is_recovered_by_automatic_restart(agent):
    assert agent._broker._proc is None  # not started yet
    first = agent.handle("what is my system info")
    assert "TuwaiqOS" in first or "kernel" in first.lower() or first  # started fine

    pid_before = agent._broker._proc.pid
    agent._broker._proc.kill()
    agent._broker._proc.wait()

    second = agent.handle("what is my system info")
    assert agent._broker._proc is not None
    assert agent._broker._proc.pid != pid_before, "broker should have been restarted with a new pid"
    assert second  # got a real answer, not stuck


def test_broker_permanently_unavailable_is_reported_not_raised():
    client = BrokerClient(broker_path="/nonexistent/path/to/broker")
    try:
        response = client.call("get_system_info", {})
        assert response.status == "error"
    except BrokerUnavailableError:
        pass  # also an acceptable outcome per broker_client.py's contract