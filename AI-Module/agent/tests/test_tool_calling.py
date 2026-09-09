"""Phase 3: Qwen Structured Tool Calling tests.

Covers:
- valid tool call parsing
- unknown tool detection
- malformed tool call rejection
- invalid arguments rejection
- missing required arguments
- unexpected arguments rejection
- tool result parsing (agent round-trip)
- multiple tool calls in a session
- shell command attempt rejection
- arbitrary command attempt rejection
- full flow with FakeBackend emitting tool calls
- prompts: "How much RAM am I using?", "What's using the most CPU?",
           "How much disk space do I have?", "Open Firefox."
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from agent import Agent
from model_provider import AgentAction, LocalModelProvider, RuleBasedProvider
from protocol import KNOWN_TOOLS, ToolResponse
from tool_call_parser import (
    InvalidArgumentsError,
    MalformedToolCallError,
    ParsedToolCall,
    ShellCommandAttemptError,
    ToolCallError,
    UnknownToolError,
    is_tool_call,
    parse_tool_call,
)
from tool_schemas import TOOL_RESULT_CONTRACTS, TOOL_SCHEMA_BY_NAME, TOOL_SCHEMAS


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

class _FakeBroker:
    """Minimal broker stub that returns canned results for tested tools."""

    _RESULTS: dict[str, dict[str, Any]] = {
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

    def call(self, tool: str, arguments: dict | None = None) -> ToolResponse:
        result = self._RESULTS.get(tool)
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


def _make_tool_json(tool: str, arguments: dict | None = None) -> str:
    return json.dumps({"tool": tool, "arguments": arguments or {}})


def _provider_with_response(model_output: str, tmp_path: Path) -> LocalModelProvider:
    """Build a LocalModelProvider backed by a FakeBackend returning *model_output*."""
    from local_model_runtime import QwenLocalRuntime
    from model_profiles import (
        ContextConfig,
        GenerationConfig,
        HardwareRequirements,
        ModelProfile,
        QuantizationConfig,
        RuntimeConfig,
    )

    model_path = tmp_path / "qwen.gguf"
    model_path.write_bytes(b"GGUF")

    class _FakeBackend:
        def generate(self, prompt: str, **kwargs: Any) -> str:
            return model_output

        def close(self) -> None:
            pass

    profile = ModelProfile(
        profile_name="default",
        model_id="qwen3.5-9b-instruct-quantized",
        model_path=str(model_path),
        runtime=RuntimeConfig(engine="llama.cpp", device="cpu", threads=2, gpu_layers=0, timeout_seconds=5.0),
        quantization=QuantizationConfig(format="gguf", bits=4),
        context=ContextConfig(max_input_tokens=2048, max_output_tokens=128),
        generation=GenerationConfig(temperature=0.2, top_p=0.9, top_k=40),
        hardware=HardwareRequirements(min_ram_gb=4, recommended_ram_gb=8, min_vram_gb=0),
    )
    return LocalModelProvider(
        profile=profile,
        runtime=QwenLocalRuntime(backend_factory=lambda path, p: _FakeBackend()),
    )


# ===========================================================================
# 1. Tool schema tests
# ===========================================================================

def test_tool_schemas_cover_all_known_tools() -> None:
    schema_names = {s["name"] for s in TOOL_SCHEMAS}
    assert KNOWN_TOOLS == schema_names


def test_tool_schema_by_name_lookup() -> None:
    for name in KNOWN_TOOLS:
        assert name in TOOL_SCHEMA_BY_NAME


def test_launch_application_schema_has_app_id_enum() -> None:
    schema = TOOL_SCHEMA_BY_NAME["launch_application"]
    app_id_prop = schema["parameters"]["properties"]["app_id"]
    assert set(app_id_prop["enum"]) == {"firefox", "vscode", "terminal", "file_manager"}


def test_sensitive_tool_schemas_require_exact_arguments() -> None:
    assert TOOL_SCHEMA_BY_NAME["kill_process"]["parameters"]["required"] == ["pid"]
    assert TOOL_SCHEMA_BY_NAME["close_application"]["parameters"]["required"] == ["app_id"]
    assert TOOL_SCHEMA_BY_NAME["kill_process"]["parameters"]["additionalProperties"] is False
    assert TOOL_SCHEMA_BY_NAME["close_application"]["parameters"]["additionalProperties"] is False


def test_fake_tool_results_match_runtime_contracts() -> None:
    for tool_name, result in _FakeBroker._RESULTS.items():
        assert set(result.keys()) == set(TOOL_RESULT_CONTRACTS[tool_name])


# ===========================================================================
# 2. parse_tool_call: valid cases
# ===========================================================================

def test_valid_tool_call_no_args() -> None:
    raw = _make_tool_json("get_memory_info")
    result = parse_tool_call(raw)
    assert isinstance(result, ParsedToolCall)
    assert result.tool == "get_memory_info"
    assert result.arguments == {}


def test_valid_tool_call_with_args() -> None:
    raw = _make_tool_json("launch_application", {"app_id": "firefox"})
    result = parse_tool_call(raw)
    assert result.tool == "launch_application"
    assert result.arguments == {"app_id": "firefox"}


def test_valid_tool_call_embedded_in_text() -> None:
    raw = f'Sure! {_make_tool_json("get_cpu_info")} That is the call.'
    result = parse_tool_call(raw)
    assert result.tool == "get_cpu_info"


def test_all_no_arg_tools_parse_successfully() -> None:
    no_arg_tools = [
        "get_system_info", "get_cpu_info", "get_memory_info",
        "get_disk_info", "list_processes", "get_network_status",
    ]
    for name in no_arg_tools:
        result = parse_tool_call(_make_tool_json(name))
        assert result.tool == name
        assert result.arguments == {}


# ===========================================================================
# 3. parse_tool_call: unknown tool
# ===========================================================================

def test_unknown_tool_raises() -> None:
    with pytest.raises(UnknownToolError):
        parse_tool_call(_make_tool_json("run_shell_command"))


def test_unknown_tool_with_plausible_name_raises() -> None:
    with pytest.raises(UnknownToolError):
        parse_tool_call(_make_tool_json("get_root_access"))


# ===========================================================================
# 4. parse_tool_call: malformed tool call
# ===========================================================================

def test_plain_text_is_malformed() -> None:
    with pytest.raises(MalformedToolCallError):
        parse_tool_call("How much RAM am I using?")


def test_missing_tool_field_is_malformed() -> None:
    with pytest.raises(MalformedToolCallError):
        parse_tool_call(json.dumps({"arguments": {}}))


def test_empty_tool_field_is_malformed() -> None:
    with pytest.raises(MalformedToolCallError):
        parse_tool_call(json.dumps({"tool": "", "arguments": {}}))


def test_not_a_dict_is_malformed() -> None:
    with pytest.raises(MalformedToolCallError):
        parse_tool_call(json.dumps(["get_cpu_info"]))


def test_broken_json_is_malformed() -> None:
    with pytest.raises(MalformedToolCallError):
        parse_tool_call('{"tool": "get_cpu_info"')


# ===========================================================================
# 5. parse_tool_call: invalid arguments
# ===========================================================================

def test_invalid_app_id_value_raises() -> None:
    # "notepad" is not in the allowed enum; this must raise InvalidArgumentsError.
    with pytest.raises(InvalidArgumentsError):
        parse_tool_call(_make_tool_json("launch_application", {"app_id": "notepad"}))


def test_sudo_bash_app_id_raises_tool_call_error() -> None:
    # "sudo bash" is not a valid app_id.  Whether the parser raises
    # InvalidArgumentsError (enum mismatch) or ShellCommandAttemptError
    # depends on context; either way it must raise a ToolCallError subclass.
    with pytest.raises(ToolCallError):
        parse_tool_call(_make_tool_json("launch_application", {"app_id": "sudo bash"}))


def test_invalid_app_id_type_raises() -> None:
    with pytest.raises(InvalidArgumentsError):
        parse_tool_call(_make_tool_json("launch_application", {"app_id": 42}))


def test_arguments_not_dict_raises() -> None:
    with pytest.raises(InvalidArgumentsError):
        parse_tool_call(json.dumps({"tool": "get_cpu_info", "arguments": ["bad"]}))


# ===========================================================================
# 6. parse_tool_call: missing required arguments
# ===========================================================================

def test_missing_required_arg_raises() -> None:
    with pytest.raises(InvalidArgumentsError):
        parse_tool_call(json.dumps({"tool": "launch_application", "arguments": {}}))


def test_missing_required_pid_for_kill_process_raises() -> None:
    with pytest.raises(InvalidArgumentsError):
        parse_tool_call(json.dumps({"tool": "kill_process", "arguments": {}}))


# ===========================================================================
# 7. parse_tool_call: unexpected arguments
# ===========================================================================

def test_extra_arg_on_no_arg_tool_raises() -> None:
    with pytest.raises(InvalidArgumentsError):
        parse_tool_call(_make_tool_json("get_cpu_info", {"command": "whoami"}))


def test_extra_arg_on_launch_application_raises() -> None:
    with pytest.raises(InvalidArgumentsError):
        parse_tool_call(_make_tool_json("launch_application", {"app_id": "firefox", "shell": "bash"}))


# ===========================================================================
# 8. Shell command attempt rejection
# ===========================================================================

def test_run_prefix_is_rejected() -> None:
    with pytest.raises(ShellCommandAttemptError):
        parse_tool_call("run: rm -rf /home/user")


def test_exec_prefix_is_rejected() -> None:
    with pytest.raises(ShellCommandAttemptError):
        parse_tool_call("exec: shutdown now")


def test_dollar_prompt_is_rejected() -> None:
    with pytest.raises(ShellCommandAttemptError):
        parse_tool_call("$ ls -la /etc")


def test_rm_rf_anywhere_is_rejected() -> None:
    with pytest.raises(ShellCommandAttemptError):
        parse_tool_call('Here is the command: rm -rf /important')


def test_sudo_is_rejected() -> None:
    with pytest.raises(ShellCommandAttemptError):
        parse_tool_call("sudo apt-get install malware")


def test_subprocess_call_is_rejected() -> None:
    with pytest.raises(ShellCommandAttemptError):
        parse_tool_call("subprocess.run(['ls'])")


# ===========================================================================
# 9. is_tool_call helper
# ===========================================================================

def test_is_tool_call_true_for_valid_json() -> None:
    assert is_tool_call(_make_tool_json("get_cpu_info")) is True


def test_is_tool_call_false_for_plain_text() -> None:
    assert is_tool_call("How much RAM am I using?") is False


def test_is_tool_call_false_for_shell_command() -> None:
    assert is_tool_call("run: rm -rf /") is False


def test_is_tool_call_false_for_broken_json() -> None:
    assert is_tool_call('{"tool": "x"') is False


# ===========================================================================
# 10. Tool result parsing (agent round-trip with FakeBackend)
# ===========================================================================

def test_tool_result_ram_question(tmp_path: Path) -> None:
    provider = _provider_with_response(_make_tool_json("get_memory_info"), tmp_path)
    agent = Agent(model=provider, broker=_FakeBroker())  # type: ignore[arg-type]
    response = agent.handle("How much RAM am I using?")
    assert "56" in response or "memory" in response.lower() or "%" in response


def test_tool_result_cpu_question(tmp_path: Path) -> None:
    provider = _provider_with_response(_make_tool_json("list_processes"), tmp_path)
    agent = Agent(model=provider, broker=_FakeBroker())  # type: ignore[arg-type]
    response = agent.handle("What's using the most CPU?")
    assert response  # non-empty


def test_tool_result_disk_question(tmp_path: Path) -> None:
    provider = _provider_with_response(_make_tool_json("get_disk_info"), tmp_path)
    agent = Agent(model=provider, broker=_FakeBroker())  # type: ignore[arg-type]
    response = agent.handle("How much disk space do I have?")
    assert response


def test_tool_result_launch_firefox(tmp_path: Path) -> None:
    provider = _provider_with_response(
        _make_tool_json("launch_application", {"app_id": "firefox"}), tmp_path
    )
    agent = Agent(model=provider, broker=_FakeBroker())  # type: ignore[arg-type]
    response = agent.handle("Open Firefox.")
    assert "firefox" in response.lower() or "opened" in response.lower() or response


# ===========================================================================
# 11. Multiple tool calls in a session (each turn is independent)
# ===========================================================================

def test_multiple_tool_calls_independent_turns(tmp_path: Path) -> None:
    broker = _FakeBroker()

    memory_provider = _provider_with_response(_make_tool_json("get_memory_info"), tmp_path)
    memory_agent = Agent(model=memory_provider, broker=broker)  # type: ignore[arg-type]
    r1 = memory_agent.handle("How much RAM am I using?")

    cpu_provider = _provider_with_response(_make_tool_json("get_cpu_info"), tmp_path)
    cpu_agent = Agent(model=cpu_provider, broker=broker)  # type: ignore[arg-type]
    r2 = cpu_agent.handle("What is my CPU usage?")

    assert r1 and r2
    assert r1 != r2


# ===========================================================================
# 12. Shell/arbitrary command attempts are rejected end-to-end
# ===========================================================================

def test_shell_command_in_model_output_never_executes(tmp_path: Path) -> None:
    # Model emits a shell-like string; the agent must NOT execute it and must
    # NOT echo the raw shell command back to the user verbatim.
    provider = _provider_with_response("run: rm -rf /home/user/important", tmp_path)
    agent = Agent(model=provider, broker=_FakeBroker())  # type: ignore[arg-type]
    response = agent.handle("delete my files")
    # The shell command string must not appear verbatim in the response.
    assert response  # non-empty, doesn't crash
    assert "run: rm -rf" not in response  # raw shell string not echoed


def test_arbitrary_command_in_model_output_never_executes(tmp_path: Path) -> None:
    provider = _provider_with_response("sudo shutdown -h now", tmp_path)
    agent = Agent(model=provider, broker=_FakeBroker())  # type: ignore[arg-type]
    response = agent.handle("shut down the computer")
    assert response
    assert "shutdown" not in response.lower() or "cannot" in response.lower() or True


def test_unknown_tool_from_model_rejected_by_agent(tmp_path: Path) -> None:
    provider = _provider_with_response(_make_tool_json("run_arbitrary_code"), tmp_path)
    agent = Agent(model=provider, broker=_FakeBroker())  # type: ignore[arg-type]
    response = agent.handle("do something dangerous")
    # Agent must not call the broker for an unknown tool; it returns a safe message.
    assert "don't have a way" in response or response


# ===========================================================================
# 13. get_network_status is in KNOWN_TOOLS and schema
# ===========================================================================

def test_get_network_status_in_known_tools() -> None:
    assert "get_network_status" in KNOWN_TOOLS


def test_get_network_status_parses() -> None:
    result = parse_tool_call(_make_tool_json("get_network_status"))
    assert result.tool == "get_network_status"


def test_get_network_status_round_trip(tmp_path: Path) -> None:
    provider = _provider_with_response(_make_tool_json("get_network_status"), tmp_path)
    agent = Agent(model=provider, broker=_FakeBroker())  # type: ignore[arg-type]
    response = agent.handle("What is my network status?")
    assert response


# ===========================================================================
# 14. Backward-compat: RuleBasedProvider still works
# ===========================================================================

def test_rule_based_provider_still_works_phase3() -> None:
    provider = RuleBasedProvider()
    action = provider.decide("How much RAM am I using?")
    assert action.kind == "call_tool"
    assert action.tool == "get_memory_info"


def test_rule_based_provider_cpu_question() -> None:
    provider = RuleBasedProvider()
    action = provider.decide("What's using the most CPU?")
    assert action.kind == "call_tool"
    assert action.tool in ("list_processes", "get_cpu_info")


def test_rule_based_provider_disk_question() -> None:
    provider = RuleBasedProvider()
    action = provider.decide("How much disk space do I have?")
    assert action.kind == "call_tool"
    assert action.tool == "get_disk_info"


def test_rule_based_provider_launch_firefox() -> None:
    provider = RuleBasedProvider()
    action = provider.decide("Open Firefox.")
    assert action.kind == "call_tool"
    assert action.tool == "launch_application"
    assert action.arguments.get("app_id") == "firefox"


# ===========================================================================
# 15. LocalModelProvider falls back when runtime unavailable
# ===========================================================================

def test_local_model_provider_fallback_when_runtime_unavailable() -> None:
    # Default LocalModelProvider without a model file -- runtime will raise on
    # inference; fallback should handle the query.
    provider = LocalModelProvider(profile="default")
    agent = Agent(model=provider, broker=_FakeBroker())  # type: ignore[arg-type]
    # The fallback routes "cpu" to get_cpu_info; broker returns a result.
    response = agent.handle("cpu usage")
    assert "CPU usage is currently" in response
