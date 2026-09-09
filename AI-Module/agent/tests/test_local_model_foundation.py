from __future__ import annotations

import time
from pathlib import Path

import pytest

from agent import Agent
from local_model_runtime import (
    IncompatibleRuntimeError,
    InvalidModelPathError,
    MissingModelError,
    ModelLoadError,
    QwenLocalRuntime,
)
from model_profiles import (
    BUILTIN_MODEL_PROFILES,
    ContextConfig,
    GenerationConfig,
    HardwareRequirements,
    ModelProfile,
    QuantizationConfig,
    RuntimeConfig,
    load_model_profile,
    resolve_model_path,
)
from model_provider import LocalModelProvider, ModelProvider, RuleBasedProvider
from protocol import ToolResponse


class _FakeBroker:
    def call(self, tool: str, arguments: dict | None = None) -> ToolResponse:
        if tool == "get_cpu_info":
            return ToolResponse(
                protocol_version="1.0",
                request_id="req-1",
                timestamp="now",
                status="ok",
                result={"usage_percent": 20.0, "core_count": 8},
            )
        return ToolResponse(
            protocol_version="1.0",
            request_id="req-1",
            timestamp="now",
            status="error",
            error={"code": "internal_error", "message": "unsupported in test broker"},
        )


class _FakeBackend:
    def __init__(self, *, response: str = "Hello from local Qwen.", error: Exception | None = None, delay: float = 0.0):
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


def _profile_for(model_path: Path, *, timeout_seconds: float = 0.1) -> ModelProfile:
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
        context=ContextConfig(max_input_tokens=2048, max_output_tokens=128),
        generation=GenerationConfig(temperature=0.2, top_p=0.9, top_k=40),
        hardware=HardwareRequirements(min_ram_gb=4, recommended_ram_gb=8, min_vram_gb=0),
    )


def test_rule_based_provider_still_works() -> None:
    provider = RuleBasedProvider()
    action = provider.decide("what is my cpu usage?")
    assert action.kind == "call_tool"
    assert action.tool == "get_cpu_info"


def test_local_model_provider_can_be_instantiated() -> None:
    provider = LocalModelProvider(profile="default")
    assert isinstance(provider, ModelProvider)
    assert provider.profile.profile_name == "default"


def test_agent_depends_on_model_provider_abstraction() -> None:
    broker = _FakeBroker()
    provider: ModelProvider = LocalModelProvider(profile="lite")
    agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
    response = agent.handle("show cpu")
    assert "CPU usage is currently" in response


def test_switching_providers_requires_no_agent_code_changes() -> None:
    broker = _FakeBroker()
    for provider in (RuleBasedProvider(), LocalModelProvider(profile="default")):
        agent = Agent(model=provider, broker=broker)  # type: ignore[arg-type]
        assert "CPU usage is currently" in agent.handle("cpu")


def test_model_profiles_can_be_selected_and_loaded() -> None:
    default_profile = load_model_profile("default")
    assert default_profile == BUILTIN_MODEL_PROFILES["default"]
    assert default_profile.model_id == "qwen3.5-9b-instruct-quantized"
    assert default_profile.runtime.engine == "llama.cpp"

    custom = load_model_profile(
        {
            "profile_name": "custom",
            "model_id": "custom-local-model",
            "model_path": "custom.gguf",
            "runtime": {
                "engine": "llama.cpp",
                "device": "cpu",
                "threads": 2,
                "gpu_layers": 0,
                "timeout_seconds": 15.0,
            },
            "quantization": {"format": "gguf", "bits": 4},
            "context": {"max_input_tokens": 2048, "max_output_tokens": 256},
            "generation": {"temperature": 0.1, "top_p": 0.95, "top_k": 40},
            "hardware": {"min_ram_gb": 4, "recommended_ram_gb": 8, "min_vram_gb": 0},
        }
    )
    assert isinstance(custom, ModelProfile)
    assert custom.profile_name == "custom"


def test_invalid_model_configuration_is_handled_safely() -> None:
    with pytest.raises(ValueError):
        load_model_profile(
            {
                "profile_name": "bad",
                "model_id": "bad-model",
                "model_path": "bad.gguf",
                "runtime": {
                    "engine": "llama.cpp",
                    "device": "cpu",
                    "threads": 0,
                    "gpu_layers": 0,
                    "timeout_seconds": 15.0,
                },
                "quantization": {"format": "gguf", "bits": 4},
                "context": {"max_input_tokens": 2048, "max_output_tokens": 256},
                "generation": {"temperature": 0.1, "top_p": 0.95, "top_k": 40},
                "hardware": {"min_ram_gb": 4, "recommended_ram_gb": 8, "min_vram_gb": 0},
            }
        )


def test_model_root_override_is_used_for_builtin_profiles(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    model_root = tmp_path / "models"
    model_root.mkdir()
    monkeypatch.setenv("TUWAIQ_AI_MODEL_ROOT", str(model_root))
    resolved = resolve_model_path(BUILTIN_MODEL_PROFILES["default"], root=Path("/unused"))
    assert resolved == (model_root / "qwen3.5-9b-quantized.gguf").resolve()


def test_qwen_runtime_initializes_tracks_telemetry_and_shutdown(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    backend = _FakeBackend()
    runtime = QwenLocalRuntime(backend_factory=lambda path, profile: backend)
    provider = LocalModelProvider(profile=_profile_for(model_path), runtime=runtime, require_model_file=True)

    provider.initialize()
    telemetry = provider.telemetry()

    assert telemetry["loaded"] is True
    assert telemetry["model_path"] == str(model_path.resolve())
    assert telemetry["load_duration_ms"] is not None
    assert telemetry["ram_usage_mb"] is not None

    provider.shutdown()

    assert provider.telemetry()["loaded"] is False
    assert backend.closed is True


def test_qwen_runtime_inference_returns_response(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    backend = _FakeBackend(response="Hello from Qwen local runtime.")
    provider = LocalModelProvider(
        profile=_profile_for(model_path),
        runtime=QwenLocalRuntime(backend_factory=lambda path, profile: backend),
    )

    action = provider.decide("Hello")

    assert action.kind == "respond"
    assert "Hello from Qwen" in (action.text or "")
    assert provider.telemetry()["inference_latency_ms"] is not None


def test_missing_model_is_reported_safely(tmp_path: Path) -> None:
    provider = LocalModelProvider(
        profile=_profile_for(tmp_path / "missing.gguf"),
        runtime=QwenLocalRuntime(backend_factory=lambda path, profile: _FakeBackend()),
    )

    with pytest.raises(MissingModelError):
        provider.initialize()

    assert provider.telemetry()["last_error_kind"] == "missing_model"


def test_invalid_model_path_is_rejected(tmp_path: Path) -> None:
    with pytest.raises(InvalidModelPathError):
        LocalModelProvider(profile=_profile_for(tmp_path / "not-a-gguf.bin"), runtime=QwenLocalRuntime())


def test_incompatible_runtime_is_rejected(tmp_path: Path) -> None:
    with pytest.raises(IncompatibleRuntimeError):
        LocalModelProvider(
            profile=ModelProfile(
                profile_name="default",
                model_id="qwen3.5-9b-instruct-quantized",
                model_path=str(tmp_path / "qwen.gguf"),
                runtime=RuntimeConfig(
                    engine="unsupported-runtime",
                    device="cpu",
                    threads=2,
                    gpu_layers=0,
                    timeout_seconds=1.0,
                ),
                quantization=QuantizationConfig(format="gguf", bits=4),
                context=ContextConfig(max_input_tokens=2048, max_output_tokens=128),
                generation=GenerationConfig(temperature=0.2, top_p=0.9, top_k=40),
                hardware=HardwareRequirements(min_ram_gb=4, recommended_ram_gb=8, min_vram_gb=0),
            ),
            runtime=QwenLocalRuntime(),
        )


def test_model_loading_failure_is_reported(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    provider = LocalModelProvider(
        profile=_profile_for(model_path),
        runtime=QwenLocalRuntime(backend_factory=lambda path, profile: (_ for _ in ()).throw(RuntimeError("load"))),
    )

    with pytest.raises(ModelLoadError):
        provider.initialize()

    assert provider.telemetry()["last_error_kind"] == "model_loading_failure"


def test_model_failure_falls_back_safely(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    provider = LocalModelProvider(
        profile=_profile_for(model_path),
        runtime=QwenLocalRuntime(backend_factory=lambda path, profile: _FakeBackend(error=RuntimeError("boom"))),
    )

    action = provider.decide("Hello")

    assert action.kind == "respond"
    assert "What would you like to know?" in (action.text or "")
    assert provider.telemetry()["last_error_kind"] == "inference_failure"


def test_timeout_falls_back_safely(tmp_path: Path) -> None:
    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")
    provider = LocalModelProvider(
        profile=_profile_for(model_path, timeout_seconds=0.01),
        runtime=QwenLocalRuntime(backend_factory=lambda path, profile: _FakeBackend(delay=0.05)),
    )

    action = provider.decide("Hello")

    assert action.kind == "respond"
    assert "What would you like to know?" in (action.text or "")
    assert provider.telemetry()["last_error_kind"] == "timeout"
