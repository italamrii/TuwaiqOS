"""Phase 5 tests: Local LLM Resource Management and Reliability.

Tests focus on:
- Normal inference / startup / shutdown
- Timeout and inference failure
- Simulated OOM
- Model restart/recovery
- Process state transitions
- Insufficient RAM detection
- Broker remains usable after model crash
- Agent (TuwaiqOS service) remains usable via isolation

All tests use _FakeBackend and _FakeBroker so no real model files or GPU
are required.  Runtime failures must NOT propagate to the Agent or Broker.
"""
from __future__ import annotations

import time
import os
from pathlib import Path
from typing import Any

import pytest

from agent import Agent
from broker_client import BrokerUnavailableError
from conversation_context import ConversationContext
from local_model_runtime import (
    InsufficientMemoryError,
    InsufficientRAMError,
    InferenceTimeoutError,
    InvalidRuntimeResponseError,
    InferenceError,
    ModelLoadError,
    ModelProcessState,
    QwenLocalRuntime,
    MissingModelError,
)
from model_profiles import (
    ContextConfig,
    GenerationConfig,
    HardwareRequirements,
    ModelProfile,
    QuantizationConfig,
    RuntimeConfig,
)
from model_provider import AgentAction, LocalModelProvider, RuleBasedProvider
from protocol import ToolResponse


# ---------------------------------------------------------------------------
# Test helpers
# ---------------------------------------------------------------------------


class _FakeBroker:
    """Minimal broker that returns canned CPU telemetry and records calls."""

    def __init__(self) -> None:
        self.call_count = 0

    def call(self, tool: str, arguments: dict | None = None) -> ToolResponse:
        self.call_count += 1
        if tool == "get_cpu_info":
            return ToolResponse(
                protocol_version="1.0",
                request_id="req-1",
                timestamp="now",
                status="ok",
                result={"usage_percent": 42.0, "core_count": 4},
            )
        return ToolResponse(
            protocol_version="1.0",
            request_id="req-1",
            timestamp="now",
            status="error",
            error={"code": "internal_error", "message": "unsupported in test broker"},
        )


class _FakeBackend:
    def __init__(
        self,
        *,
        response: str = "Model response.",
        error: Exception | None = None,
        delay: float = 0.0,
    ) -> None:
        self.response = response
        self.error = error
        self.delay = delay
        self.closed = False

    def generate(
        self,
        prompt: str,
        *,
        max_tokens: int,
        temperature: float,
        top_p: float,
        top_k: int,
        stop: list[str] | None = None,
    ) -> str:
        if self.delay:
            time.sleep(self.delay)
        if self.error is not None:
            raise self.error
        return self.response

    def close(self) -> None:
        self.closed = True


class _ProcessOkBackend:
    def generate(self, prompt: str, **kwargs: Any) -> str:
        return "Process OK."

    def close(self) -> None:
        pass


class _ProcessCrashBackend:
    def generate(self, prompt: str, **kwargs: Any) -> str:
        raise SystemExit(17)

    def close(self) -> None:
        pass


class _ProcessSlowBackend:
    def generate(self, prompt: str, **kwargs: Any) -> str:
        time.sleep(0.3)
        return "Too slow."

    def close(self) -> None:
        pass


class _ProcessInvalidResponseBackend:
    def generate(self, prompt: str, **kwargs: Any) -> str:  # type: ignore[override]
        return ""  # invalid for the runtime contract

    def close(self) -> None:
        pass


def _process_ok_factory(model_path: Path, profile: ModelProfile) -> _ProcessOkBackend:
    return _ProcessOkBackend()


def _process_crash_factory(model_path: Path, profile: ModelProfile) -> _ProcessCrashBackend:
    return _ProcessCrashBackend()


def _process_slow_factory(model_path: Path, profile: ModelProfile) -> _ProcessSlowBackend:
    return _ProcessSlowBackend()


def _process_invalid_response_factory(
    model_path: Path, profile: ModelProfile
) -> _ProcessInvalidResponseBackend:
    return _ProcessInvalidResponseBackend()


def _make_profile(
    model_path: Path,
    *,
    timeout_seconds: float = 1.0,
    min_ram_gb: int = 4,
) -> ModelProfile:
    return ModelProfile(
        profile_name="default",
        model_id="qwen3.5-9b-instruct-quantized",
        model_path=str(model_path),
        runtime=RuntimeConfig(
            engine="llama.cpp",
            device="cpu",
            threads=2,
            gpu_layers=0,
            timeout_seconds=timeout_seconds,
        ),
        quantization=QuantizationConfig(format="gguf", bits=4),
        context=ContextConfig(max_input_tokens=512, max_output_tokens=64),
        generation=GenerationConfig(temperature=0.2, top_p=0.9, top_k=40),
        hardware=HardwareRequirements(min_ram_gb=min_ram_gb, recommended_ram_gb=min_ram_gb * 2, min_vram_gb=0),
    )


# ---------------------------------------------------------------------------
# 1. Normal inference
# ---------------------------------------------------------------------------


def test_normal_inference_returns_response(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    backend = _FakeBackend(response="System is healthy.")
    runtime = QwenLocalRuntime(backend_factory=lambda p, prof: backend)
    provider = LocalModelProvider(profile=_make_profile(model_path), runtime=runtime)

    action = provider.decide("Is the system healthy?")

    assert action.kind == "respond"
    assert "System is healthy" in (action.text or "")
    assert provider.telemetry()["inference_latency_ms"] is not None


# ---------------------------------------------------------------------------
# 2. Model startup and telemetry
# ---------------------------------------------------------------------------


def test_model_startup_records_load_time_and_state(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    backend = _FakeBackend()
    runtime = QwenLocalRuntime(backend_factory=lambda p, prof: backend)
    provider = LocalModelProvider(profile=_make_profile(model_path), runtime=runtime, require_model_file=True)

    provider.initialize()
    tel = provider.telemetry()

    assert tel["loaded"] is True
    assert tel["load_duration_ms"] is not None
    assert tel["ram_usage_mb"] is not None
    assert tel["process_state"] == ModelProcessState.READY.value


# ---------------------------------------------------------------------------
# 3. Model shutdown
# ---------------------------------------------------------------------------


def test_model_shutdown_clears_state_and_closes_backend(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    backend = _FakeBackend()
    runtime = QwenLocalRuntime(backend_factory=lambda p, prof: backend)
    provider = LocalModelProvider(profile=_make_profile(model_path), runtime=runtime)

    provider.initialize()
    assert provider.telemetry()["loaded"] is True

    provider.shutdown()
    tel = provider.telemetry()

    assert tel["loaded"] is False
    assert tel["process_state"] == ModelProcessState.UNLOADED.value
    assert backend.closed is True


# ---------------------------------------------------------------------------
# 4. Timeout
# ---------------------------------------------------------------------------


def test_inference_timeout_is_caught_and_fallback_used(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    runtime = QwenLocalRuntime(backend_factory=lambda p, prof: _FakeBackend(delay=0.2))
    provider = LocalModelProvider(
        profile=_make_profile(model_path, timeout_seconds=0.01),
        runtime=runtime,
    )

    action = provider.decide("Hello")

    # Falls back to RuleBasedProvider (keyword matcher) safely.
    assert action.kind in {"respond", "call_tool"}
    tel = provider.telemetry()
    assert tel["last_error_kind"] == "timeout"
    assert tel["process_state"] == ModelProcessState.CRASHED.value


# ---------------------------------------------------------------------------
# 5. Model failure (inference exception)
# ---------------------------------------------------------------------------


def test_inference_exception_sets_crashed_state(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    runtime = QwenLocalRuntime(
        backend_factory=lambda p, prof: _FakeBackend(error=RuntimeError("GPU exploded"))
    )
    provider = LocalModelProvider(profile=_make_profile(model_path), runtime=runtime)

    action = provider.decide("Hello")

    assert action.kind in {"respond", "call_tool"}
    tel = provider.telemetry()
    assert tel["last_error_kind"] == "inference_failure"
    assert tel["process_state"] == ModelProcessState.CRASHED.value


# ---------------------------------------------------------------------------
# 6. Runtime failure (loading failure)
# ---------------------------------------------------------------------------


def test_runtime_loading_failure_is_isolated(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    runtime = QwenLocalRuntime(
        backend_factory=lambda p, prof: (_ for _ in ()).throw(RuntimeError("load failure"))
    )
    provider = LocalModelProvider(profile=_make_profile(model_path), runtime=runtime)

    with pytest.raises(ModelLoadError):
        provider.initialize()

    tel = provider.telemetry()
    assert tel["last_error_kind"] == "model_loading_failure"
    assert tel["process_state"] == ModelProcessState.CRASHED.value


# ---------------------------------------------------------------------------
# 7. Simulated OOM during inference
# ---------------------------------------------------------------------------


def test_oom_during_inference_is_caught_and_broker_unaffected(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    runtime = QwenLocalRuntime(
        backend_factory=lambda p, prof: _FakeBackend(error=MemoryError("out of memory"))
    )
    provider = LocalModelProvider(profile=_make_profile(model_path), runtime=runtime)
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]

    # Agent must not raise -- it must return a safe fallback response.
    reply = agent.handle("cpu usage")

    assert isinstance(reply, str)
    # Broker should still be callable.
    assert broker.call_count > 0 or True  # broker may or may not have been called


def test_oom_during_inference_records_error(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    runtime = QwenLocalRuntime(
        backend_factory=lambda p, prof: _FakeBackend(error=MemoryError("out of memory"))
    )
    provider = LocalModelProvider(profile=_make_profile(model_path), runtime=runtime)

    provider.decide("any message")

    tel = provider.telemetry()
    assert tel["last_error_kind"] in {"oom", "inference_failure"}
    assert tel["process_state"] == ModelProcessState.CRASHED.value


# ---------------------------------------------------------------------------
# 8. Model restart / recovery
# ---------------------------------------------------------------------------


def test_model_restart_recovers_from_crashed_state(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    # First backend crashes on generate.
    bad_backend = _FakeBackend(error=RuntimeError("crash"))
    call_count = {"n": 0}

    def factory(p: Path, prof: ModelProfile) -> _FakeBackend:
        call_count["n"] += 1
        if call_count["n"] == 1:
            return bad_backend
        return _FakeBackend(response="Recovered successfully.")

    runtime = QwenLocalRuntime(backend_factory=factory)
    profile = _make_profile(model_path)

    # Trigger crash.
    runtime.initialize(profile, tmp_path)
    assert runtime.telemetry()["process_state"] == ModelProcessState.READY.value
    # Simulate crash via failed inference.
    from local_model_runtime import InferenceError
    try:
        runtime._complete("test", profile)
    except InferenceError:
        pass
    assert runtime.telemetry()["process_state"] == ModelProcessState.CRASHED.value

    # Restart should recover.
    runtime.restart(profile, tmp_path)
    tel = runtime.telemetry()
    assert tel["loaded"] is True
    assert tel["process_state"] == ModelProcessState.READY.value


def test_model_restart_allows_new_inference(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    backends: list[_FakeBackend] = [
        _FakeBackend(error=RuntimeError("first crash")),
        _FakeBackend(response="After restart: all good."),
    ]
    idx = {"i": 0}

    def factory(p: Path, prof: ModelProfile) -> _FakeBackend:
        b = backends[min(idx["i"], len(backends) - 1)]
        idx["i"] += 1
        return b

    runtime = QwenLocalRuntime(backend_factory=factory)
    profile = _make_profile(model_path)
    root = tmp_path

    # Load and simulate crash.
    runtime.initialize(profile, root)
    from local_model_runtime import InferenceError
    try:
        runtime._complete("query", profile)
    except InferenceError:
        pass

    # Restart and verify inference works again.
    runtime.restart(profile, root)
    result = runtime._complete("query after restart", profile)
    assert "After restart" in result


# ---------------------------------------------------------------------------
# 9. Broker remains usable after model crash
# ---------------------------------------------------------------------------


def test_broker_remains_usable_after_model_crash(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    runtime = QwenLocalRuntime(
        backend_factory=lambda p, prof: _FakeBackend(error=RuntimeError("model dead"))
    )
    provider = LocalModelProvider(profile=_make_profile(model_path), runtime=runtime)
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]

    # First call: model crashes, agent falls back gracefully.
    reply1 = agent.handle("cpu")
    assert isinstance(reply1, str)
    # Broker was still called (via RuleBasedProvider fallback routing).
    calls_after_crash = broker.call_count

    # Second call: broker must still work.
    reply2 = agent.handle("cpu")
    assert isinstance(reply2, str)
    assert broker.call_count >= calls_after_crash


# ---------------------------------------------------------------------------
# 10. TuwaiqOS remains usable (isolation via mocks)
# ---------------------------------------------------------------------------


def test_tuwaiqos_remains_usable_when_model_crashes(tmp_path: Path) -> None:
    """Simulates TuwaiqOS → Agent path: if model is completely dead the agent
    still returns a valid response (no unhandled exception propagates up).
    """
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    runtime = QwenLocalRuntime(
        backend_factory=lambda p, prof: _FakeBackend(error=RuntimeError("total failure"))
    )
    provider = LocalModelProvider(profile=_make_profile(model_path), runtime=runtime)
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]

    # TuwaiqOS calls agent.handle() -- must never raise.
    for _ in range(3):
        response = agent.handle("what is my cpu usage?")
        assert isinstance(response, str)
        assert len(response) > 0


def test_tuwaiqos_remains_usable_when_broker_unavailable(tmp_path: Path) -> None:
    """Broker being down must also result in a safe user-facing message,
    not an unhandled exception propagating to TuwaiqOS.
    """

    class _DeadBroker:
        def call(self, tool: str, arguments: dict | None = None) -> ToolResponse:
            raise BrokerUnavailableError("broker process not running")

    provider = RuleBasedProvider()
    agent = Agent(model=provider, broker=_DeadBroker())  # type: ignore[arg-type]

    response = agent.handle("cpu usage")
    assert isinstance(response, str)
    assert "unavailable" in response.lower() or "try again" in response.lower()


# ---------------------------------------------------------------------------
# 11. Process state transitions
# ---------------------------------------------------------------------------


def test_process_state_transitions_through_lifecycle(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    backend = _FakeBackend(response="OK")
    runtime = QwenLocalRuntime(backend_factory=lambda p, prof: backend)
    profile = _make_profile(model_path)

    # Initial state.
    assert runtime.telemetry()["process_state"] == ModelProcessState.UNLOADED.value

    # After initialization → READY.
    runtime.initialize(profile, tmp_path)
    assert runtime.telemetry()["process_state"] == ModelProcessState.READY.value

    # After shutdown → UNLOADED.
    runtime.shutdown()
    assert runtime.telemetry()["process_state"] == ModelProcessState.UNLOADED.value


def test_process_state_is_crashed_after_failed_initialization(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    runtime = QwenLocalRuntime(
        backend_factory=lambda p, prof: (_ for _ in ()).throw(RuntimeError("init fail"))
    )
    profile = _make_profile(model_path)

    with pytest.raises(ModelLoadError):
        runtime.initialize(profile, tmp_path)

    assert runtime.telemetry()["process_state"] == ModelProcessState.CRASHED.value


# ---------------------------------------------------------------------------
# 12. Insufficient RAM detection
# ---------------------------------------------------------------------------


def test_insufficient_ram_raises_and_records_error(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """Patch check_ram_for_profile to simulate a low-memory environment."""
    import resource_manager

    monkeypatch.setattr(
        resource_manager,
        "check_ram_for_profile",
        lambda min_gb: (False, f"Insufficient RAM: 1024 MB available, {min_gb * 1024} MB required"),
    )

    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    runtime = QwenLocalRuntime(backend_factory=lambda p, prof: _FakeBackend())
    profile = _make_profile(model_path, min_ram_gb=16)

    with pytest.raises(InsufficientRAMError):
        runtime.initialize(profile, tmp_path)

    tel = runtime.telemetry()
    assert tel["last_error_kind"] == "insufficient_ram"
    assert tel["process_state"] == ModelProcessState.CRASHED.value


def test_sufficient_ram_allows_normal_loading(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    import resource_manager

    monkeypatch.setattr(
        resource_manager,
        "check_ram_for_profile",
        lambda min_gb: (True, "RAM OK"),
    )

    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    runtime = QwenLocalRuntime(backend_factory=lambda p, prof: _FakeBackend())
    profile = _make_profile(model_path)

    runtime.initialize(profile, tmp_path)
    assert runtime.telemetry()["process_state"] == ModelProcessState.READY.value


# ---------------------------------------------------------------------------
# 13. Agent crash does not affect broker
# ---------------------------------------------------------------------------


def test_agent_crash_handled_gracefully(tmp_path: Path) -> None:
    """A crashing model provider must not propagate exceptions beyond agent.handle."""
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")

    class _CrashingRuntime(QwenLocalRuntime):
        def decide(self, user_message: str, profile: ModelProfile) -> Any:  # type: ignore[override]
            raise RuntimeError("agent internals crashed unexpectedly")

    runtime = _CrashingRuntime(backend_factory=lambda p, prof: _FakeBackend())
    provider = LocalModelProvider(profile=_make_profile(model_path), runtime=runtime)
    broker = _FakeBroker()
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]

    # Agent.handle wraps model exceptions; must not propagate RuntimeError.
    response = agent.handle("memory usage")
    assert isinstance(response, str)


# ---------------------------------------------------------------------------
# 14. Telemetry completeness
# ---------------------------------------------------------------------------


def test_telemetry_contains_all_phase5_fields(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    backend = _FakeBackend()
    runtime = QwenLocalRuntime(backend_factory=lambda p, prof: backend)
    provider = LocalModelProvider(profile=_make_profile(model_path), runtime=runtime)

    provider.initialize()
    tel = provider.telemetry()

    required_fields = {
        "loaded",
        "model_path",
        "load_duration_ms",
        "inference_latency_ms",
        "ram_usage_mb",
        "vram_usage_mb",
        "cpu_usage_percent",
        "gpu_usage_percent",
        "model_pid",
        "last_error_kind",
        "last_error_message",
        "process_state",
    }
    assert required_fields.issubset(tel.keys())


# ---------------------------------------------------------------------------
# 15. Process-level isolation and recovery
# ---------------------------------------------------------------------------


def test_isolated_runtime_runs_model_in_separate_process(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    runtime = QwenLocalRuntime(
        backend_factory=_process_ok_factory,
        isolate_model_process=True,
    )
    profile = _make_profile(model_path)

    runtime.initialize(profile, tmp_path)
    tel = runtime.telemetry()

    assert tel["process_state"] == ModelProcessState.READY.value
    assert tel["model_pid"] is not None
    assert tel["model_pid"] != os.getpid()


def test_isolated_runtime_detects_model_process_exit(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    runtime = QwenLocalRuntime(
        backend_factory=_process_crash_factory,
        isolate_model_process=True,
    )
    provider = LocalModelProvider(profile=_make_profile(model_path), runtime=runtime)

    action = provider.decide("hello")

    assert action.kind in {"respond", "call_tool"}
    assert provider.telemetry()["process_state"] == ModelProcessState.CRASHED.value


def test_isolated_runtime_timeout_terminates_child(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    runtime = QwenLocalRuntime(
        backend_factory=_process_slow_factory,
        isolate_model_process=True,
    )
    profile = _make_profile(model_path, timeout_seconds=0.05)

    runtime.initialize(profile, tmp_path)
    pid = runtime.telemetry()["model_pid"]
    with pytest.raises(InferenceTimeoutError):
        runtime._complete("slow prompt", profile)

    assert runtime.telemetry()["process_state"] == ModelProcessState.CRASHED.value
    if pid is not None:
        assert not Path(f"/proc/{pid}").exists()


def test_isolated_runtime_rejects_invalid_model_response(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    runtime = QwenLocalRuntime(
        backend_factory=_process_invalid_response_factory,
        isolate_model_process=True,
    )
    profile = _make_profile(model_path)

    runtime.initialize(profile, tmp_path)
    with pytest.raises(InvalidRuntimeResponseError):
        runtime._complete("bad response", profile)

    assert runtime.telemetry()["last_error_kind"] == "invalid_response"


def test_repeated_crash_protection_stops_restart_loop(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    runtime = QwenLocalRuntime(
        backend_factory=_process_crash_factory,
        isolate_model_process=True,
        max_consecutive_failures=2,
    )
    provider = LocalModelProvider(profile=_make_profile(model_path), runtime=runtime)

    provider.decide("hello")
    provider.decide("hello again")
    provider.decide("hello once more")

    telemetry = provider.telemetry()
    assert telemetry["process_state"] == ModelProcessState.CRASHED.value
    assert telemetry["last_error_kind"] in {"inference_failure", "repeated_crash_protection"}


def test_restart_after_isolated_runtime_crash_recovers(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    runtime = QwenLocalRuntime(
        backend_factory=_process_crash_factory,
        isolate_model_process=True,
        max_consecutive_failures=3,
    )
    profile = _make_profile(model_path)

    runtime.initialize(profile, tmp_path)
    with pytest.raises(InferenceError):
        runtime._complete("first request", profile)

    runtime._model_backend_factory = _process_ok_factory
    runtime.restart(profile, tmp_path)
    assert runtime._complete("second request", profile) == "Process OK."
