from __future__ import annotations

import enum
import multiprocessing
import os
import sys
import time
from abc import ABC, abstractmethod
from concurrent.futures import ThreadPoolExecutor, TimeoutError as FutureTimeoutError
from dataclasses import asdict, dataclass
from pathlib import Path
import resource
from typing import TYPE_CHECKING, Any, Callable, Protocol

from model_profiles import ModelProfile, load_model_profile, resolve_model_path

if TYPE_CHECKING:
    from conversation_context import ConversationContext, ToolResultEntry


class LocalRuntimeError(RuntimeError):
    """Base class for local runtime failures."""


class MissingModelError(LocalRuntimeError):
    """Raised when the configured model file is missing."""


class InvalidModelPathError(LocalRuntimeError):
    """Raised when the configured model path is invalid for the runtime."""


class IncompatibleRuntimeError(LocalRuntimeError):
    """Raised when the selected runtime cannot load the configured model."""


class ModelLoadError(LocalRuntimeError):
    """Raised when the model cannot be initialized."""


class InferenceError(LocalRuntimeError):
    """Raised when inference fails."""


class InferenceTimeoutError(InferenceError):
    """Raised when inference exceeds the configured timeout."""


class BrokenIPCError(LocalRuntimeError):
    """Raised when the isolated runtime pipe/socket becomes unusable."""


class InvalidRuntimeResponseError(InferenceError):
    """Raised when the isolated runtime returns an invalid response payload."""


class InsufficientMemoryError(ModelLoadError):
    """Raised when the runtime cannot allocate enough memory."""


class InsufficientRAMError(InsufficientMemoryError):
    """Raised when the system does not have enough RAM to load the model."""


class InsufficientVRAMError(InsufficientMemoryError):
    """Raised when the system does not have enough VRAM for GPU-accelerated inference."""


class ModelProcessState(enum.Enum):
    """Lifecycle state of the local model runtime."""

    UNLOADED = "unloaded"
    LOADING = "loading"
    READY = "ready"
    CRASHED = "crashed"


@dataclass
class LocalRuntimeTelemetry:
    loaded: bool = False
    backend: str | None = None
    device: str | None = None
    model_path: str | None = None
    load_duration_ms: float | None = None
    inference_latency_ms: float | None = None
    ram_usage_mb: float | None = None
    vram_usage_mb: float | None = None
    cpu_usage_percent: float | None = None
    gpu_usage_percent: float | None = None
    model_pid: int | None = None
    last_error_kind: str | None = None
    last_error_message: str | None = None
    process_state: str = ModelProcessState.UNLOADED.value

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


class CompletionBackend(Protocol):
    def generate(
        self,
        prompt: str,
        *,
        max_tokens: int,
        temperature: float,
        top_p: float,
        top_k: int,
        stop: list[str] | None = None,
    ) -> str: ...

    def close(self) -> None: ...


class LocalModelRuntime(ABC):
    """Runtime adapter for local model execution."""

    @abstractmethod
    def initialize(self, profile: ModelProfile, root: Path) -> None:
        """Load or prepare the selected model."""

    @abstractmethod
    def shutdown(self) -> None:
        """Release runtime resources."""

    @abstractmethod
    def telemetry(self) -> dict[str, Any]:
        """Return load/inference telemetry for the runtime."""

    @abstractmethod
    def decide(self, user_message: str, profile: ModelProfile) -> Any | None:
        """Return an AgentAction-compatible object or None to delegate."""

    def decide_next(
        self,
        user_message: str,
        accumulated: "list[ToolResultEntry]",
        context: "ConversationContext",
        profile: ModelProfile,
    ) -> Any | None:
        """Multi-step decision given accumulated results.  Default: delegate."""
        return self.decide(user_message, profile)

    def synthesize(
        self,
        user_message: str,
        accumulated: "list[ToolResultEntry]",
        context: "ConversationContext",
        profile: ModelProfile,
    ) -> str | None:
        """Synthesize answer from multiple results.  Default: delegate (None)."""
        return None

    @abstractmethod
    def explain(self, user_message: str, tool: str, result: dict[str, Any], profile: ModelProfile) -> str | None:
        """Return a natural-language explanation or None to delegate."""

    @abstractmethod
    def explain_error(
        self,
        user_message: str,
        tool: str,
        error_code: str,
        error_message: str,
        profile: ModelProfile,
    ) -> str | None:
        """Return an error explanation or None to delegate."""

    @abstractmethod
    def validate(self, profile: ModelProfile, root: Path) -> None:
        """Validate runtime prerequisites for the selected profile."""


class NoOpLocalRuntime(LocalModelRuntime):
    """Placeholder runtime used in tests and as a safe fallback."""

    def __init__(self) -> None:
        self._telemetry = LocalRuntimeTelemetry(loaded=False, backend="noop", device="cpu")

    def initialize(self, profile: ModelProfile, root: Path) -> None:
        self.validate(profile, root)

    def shutdown(self) -> None:
        self._telemetry.loaded = False

    def telemetry(self) -> dict[str, Any]:
        return self._telemetry.to_dict()

    def decide(self, user_message: str, profile: ModelProfile) -> Any | None:
        return None

    def explain(self, user_message: str, tool: str, result: dict[str, Any], profile: ModelProfile) -> str | None:
        return None

    def explain_error(
        self,
        user_message: str,
        tool: str,
        error_code: str,
        error_message: str,
        profile: ModelProfile,
    ) -> str | None:
        return None

    def validate(self, profile: ModelProfile, root: Path) -> None:
        resolve_model_path(profile, root=root)


class LlamaCppBackend:
    def __init__(self, model_path: Path, profile: ModelProfile) -> None:
        try:
            from llama_cpp import Llama
        except ImportError as exc:
            raise IncompatibleRuntimeError(
                "llama-cpp-python is required to run local Qwen GGUF models"
            ) from exc

        try:
            self._llm = Llama(
                model_path=str(model_path),
                n_ctx=profile.context.max_input_tokens + profile.context.max_output_tokens,
                n_threads=profile.runtime.threads,
                n_gpu_layers=profile.runtime.gpu_layers,
                verbose=False,
            )
        except MemoryError as exc:
            raise InsufficientMemoryError("insufficient memory while loading the local Qwen model") from exc
        except Exception as exc:
            raise ModelLoadError(f"failed to load local model from {model_path}") from exc

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
        try:
            response = self._llm.create_completion(
                prompt=prompt,
                max_tokens=max_tokens,
                temperature=temperature,
                top_p=top_p,
                top_k=top_k,
                stop=stop,
            )
        except Exception as exc:
            raise InferenceError("local Qwen inference failed") from exc

        choices = response.get("choices") or []
        if not choices:
            raise InferenceError("local Qwen runtime returned no completion choices")
        text = str(choices[0].get("text", "")).strip()
        if not text:
            raise InferenceError("local Qwen runtime returned an empty completion")
        return text

    def close(self) -> None:
        close = getattr(self._llm, "close", None)
        if callable(close):
            close()


def _worker_error_kind(exc: Exception) -> str:
    if isinstance(exc, MissingModelError):
        return "missing_model"
    if isinstance(exc, InvalidModelPathError):
        return "invalid_model_path"
    if isinstance(exc, IncompatibleRuntimeError):
        return "incompatible_runtime"
    if isinstance(exc, InsufficientRAMError):
        return "insufficient_ram"
    if isinstance(exc, InsufficientVRAMError):
        return "insufficient_vram"
    if isinstance(exc, InsufficientMemoryError):
        return "insufficient_memory"
    if isinstance(exc, InferenceTimeoutError):
        return "timeout"
    if isinstance(exc, InvalidRuntimeResponseError):
        return "invalid_response"
    if isinstance(exc, BrokenIPCError):
        return "broken_ipc"
    if isinstance(exc, ModelLoadError):
        return "model_loading_failure"
    if isinstance(exc, InferenceError):
        return "inference_failure"
    return "runtime_error"


def _runtime_worker(
    conn: Any,
    model_path: str,
    profile_data: dict[str, Any],
    backend_factory: Callable[[Path, ModelProfile], CompletionBackend],
) -> None:
    backend: CompletionBackend | None = None
    try:
        profile = load_model_profile(profile_data)
        backend = backend_factory(Path(model_path), profile)
        conn.send({"type": "ready", "pid": os.getpid()})
        while True:
            try:
                request = conn.recv()
            except EOFError:
                break

            if not isinstance(request, dict):
                conn.send(
                    {
                        "type": "error",
                        "kind": "invalid_request",
                        "message": "isolated runtime received a malformed request",
                    }
                )
                continue

            command = request.get("command")
            if command == "health":
                conn.send({"type": "health", "ok": True, "pid": os.getpid()})
                continue
            if command == "shutdown":
                conn.send({"type": "shutdown", "ok": True})
                break
            if command != "generate":
                conn.send(
                    {
                        "type": "error",
                        "kind": "invalid_request",
                        "message": f"unsupported runtime command: {command}",
                    }
                )
                continue

            try:
                text = backend.generate(
                    str(request.get("prompt", "")),
                    max_tokens=int(request["max_tokens"]),
                    temperature=float(request["temperature"]),
                    top_p=float(request["top_p"]),
                    top_k=int(request["top_k"]),
                    stop=request.get("stop"),
                )
                if not isinstance(text, str) or not text.strip():
                    raise InvalidRuntimeResponseError(
                        "local model runtime returned an invalid completion response"
                    )
                conn.send({"type": "result", "text": text})
            except MemoryError as exc:
                conn.send(
                    {
                        "type": "error",
                        "kind": "oom",
                        "message": "OOM during inference",
                        "detail": str(exc),
                    }
                )
            except Exception as exc:
                conn.send(
                    {
                        "type": "error",
                        "kind": _worker_error_kind(exc),
                        "message": str(exc),
                    }
                )
    except MemoryError as exc:
        conn.send(
            {
                "type": "error",
                "kind": "insufficient_memory",
                "message": "insufficient memory while loading the local Qwen model",
                "detail": str(exc),
            }
        )
    except Exception as exc:
        conn.send(
            {
                "type": "error",
                "kind": _worker_error_kind(exc),
                "message": str(exc),
            }
        )
    finally:
        if backend is not None:
            close = getattr(backend, "close", None)
            if callable(close):
                close()
        conn.close()


class IsolatedCompletionBackend:
    """Run model inference in a dedicated child process behind a narrow IPC API."""

    def __init__(
        self,
        model_path: Path,
        profile: ModelProfile,
        *,
        backend_factory: Callable[[Path, ModelProfile], CompletionBackend],
        start_method: str | None = None,
    ) -> None:
        available = multiprocessing.get_all_start_methods()
        selected_method = start_method or ("spawn" if "spawn" in available else available[0])
        ctx = multiprocessing.get_context(selected_method)
        parent_conn, child_conn = ctx.Pipe()
        proc = ctx.Process(
            target=_runtime_worker,
            args=(child_conn, str(model_path), profile.to_dict(), backend_factory),
            daemon=True,
        )
        proc.start()
        child_conn.close()
        self._conn = parent_conn
        self._proc = proc
        self._timeout = profile.runtime.timeout_seconds
        self._startup_timeout = min(max(profile.runtime.timeout_seconds, 1.0), 30.0)
        message = self._receive(self._startup_timeout, phase="startup")
        if message.get("type") != "ready":
            self._terminate()
            self._raise_from_message(message, default_kind="model_loading_failure")

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
        self.health_check()
        try:
            self._conn.send(
                {
                    "command": "generate",
                    "prompt": prompt,
                    "max_tokens": max_tokens,
                    "temperature": temperature,
                    "top_p": top_p,
                    "top_k": top_k,
                    "stop": stop,
                }
            )
        except (BrokenPipeError, EOFError, OSError) as exc:
            self._terminate()
            raise BrokenIPCError("isolated model runtime IPC write failed") from exc

        message = self._receive(self._timeout, phase="inference")
        if message.get("type") == "result":
            text = message.get("text")
            if not isinstance(text, str) or not text.strip():
                self._terminate()
                raise InvalidRuntimeResponseError(
                    "local model runtime returned an invalid completion response"
                )
            return text
        self._raise_from_message(message, default_kind="inference_failure")

    def close(self) -> None:
        if self._proc is None:
            return
        try:
            if self._conn is not None and self._proc.is_alive():
                self._conn.send({"command": "shutdown"})
                if self._conn.poll(1.0):
                    self._conn.recv()
        except Exception:
            pass
        finally:
            self._terminate()

    def health_check(self) -> bool:
        if self._proc is None or not self._proc.is_alive():
            raise ModelLoadError("isolated model runtime process is not available")
        try:
            self._conn.send({"command": "health"})
        except (BrokenPipeError, EOFError, OSError) as exc:
            self._terminate()
            raise BrokenIPCError("isolated model runtime IPC health check failed") from exc
        message = self._receive(1.0, phase="health")
        if message.get("type") != "health" or not message.get("ok"):
            self._terminate()
            raise ModelLoadError("isolated model runtime health check failed")
        return True

    def pid(self) -> int | None:
        if self._proc is None:
            return None
        return self._proc.pid

    def cpu_seconds(self) -> float:
        pid = self.pid()
        if pid is None:
            return 0.0
        stats = _read_linux_process_stats(pid)
        return stats["cpu_seconds"] if stats is not None else 0.0

    def ram_usage_mb(self) -> float | None:
        pid = self.pid()
        if pid is None:
            return None
        stats = _read_linux_process_stats(pid)
        return stats["rss_mb"] if stats is not None else None

    def _receive(self, timeout: float, *, phase: str) -> dict[str, Any]:
        if self._proc is None or self._conn is None:
            raise BrokenIPCError("isolated model runtime IPC channel is unavailable")
        if not self._conn.poll(timeout):
            if not self._proc.is_alive():
                raise InferenceError("isolated model runtime exited unexpectedly")
            self._terminate()
            if phase == "startup":
                raise ModelLoadError("local model runtime startup timed out")
            raise InferenceTimeoutError("local Qwen inference timed out")
        try:
            message = self._conn.recv()
        except EOFError as exc:
            self._terminate()
            raise InferenceError("isolated model runtime exited unexpectedly") from exc
        if not isinstance(message, dict):
            self._terminate()
            raise InvalidRuntimeResponseError("isolated model runtime returned a malformed response")
        return message

    def _raise_from_message(self, message: dict[str, Any], *, default_kind: str) -> None:
        kind = str(message.get("kind") or default_kind)
        text = str(message.get("message") or "local model runtime failed")
        self._terminate()
        if kind in {"missing_model"}:
            raise MissingModelError(text)
        if kind in {"invalid_model_path"}:
            raise InvalidModelPathError(text)
        if kind in {"incompatible_runtime"}:
            raise IncompatibleRuntimeError(text)
        if kind in {"insufficient_ram"}:
            raise InsufficientRAMError(text)
        if kind in {"insufficient_vram"}:
            raise InsufficientVRAMError(text)
        if kind in {"insufficient_memory"}:
            raise InsufficientMemoryError(text)
        if kind in {"oom"}:
            raise InsufficientMemoryError(text)
        if kind in {"timeout"}:
            raise InferenceTimeoutError(text)
        if kind in {"invalid_response"}:
            raise InvalidRuntimeResponseError(text)
        if kind in {"broken_ipc"}:
            raise BrokenIPCError(text)
        if default_kind == "model_loading_failure":
            raise ModelLoadError(text)
        raise InferenceError(text)

    def _terminate(self) -> None:
        if self._proc is not None:
            if self._proc.is_alive():
                self._proc.terminate()
                self._proc.join(timeout=2.0)
                if self._proc.is_alive():
                    self._proc.kill()
                    self._proc.join(timeout=1.0)
            self._proc = None
        if self._conn is not None:
            try:
                self._conn.close()
            except Exception:
                pass
            self._conn = None


def _read_linux_process_stats(pid: int) -> dict[str, float] | None:
    if pid <= 0 or sys.platform == "win32":
        return None
    try:
        stat_fields = Path(f"/proc/{pid}/stat").read_text(encoding="utf-8").split()
        status_lines = Path(f"/proc/{pid}/status").read_text(encoding="utf-8").splitlines()
    except OSError:
        return None

    page_size = os.sysconf("SC_PAGE_SIZE") if hasattr(os, "sysconf") else 4096
    ticks_per_second = os.sysconf("SC_CLK_TCK") if hasattr(os, "sysconf") else 100
    cpu_seconds = (
        float(stat_fields[13]) + float(stat_fields[14]) if len(stat_fields) > 14 else 0.0
    ) / float(ticks_per_second)
    rss_kb = 0.0
    for line in status_lines:
        if line.startswith("VmRSS:"):
            parts = line.split()
            if len(parts) >= 2:
                rss_kb = float(parts[1])
            break
    if rss_kb == 0.0 and len(stat_fields) > 23:
        rss_pages = float(stat_fields[23])
        rss_kb = (rss_pages * float(page_size)) / 1024.0
    return {
        "cpu_seconds": cpu_seconds,
        "rss_mb": rss_kb / 1024.0,
    }


class QwenLocalRuntime(LocalModelRuntime):
    """Local GGUF runtime for Qwen profiles via llama.cpp."""

    _SUPPORTED_ENGINES = {"llama.cpp", "llama_cpp"}
    _STOP_TOKENS = ["<|im_end|>", "<|endoftext|>"]
    _SYSTEM_PROMPT = (
        "You are Tuwaiq AI running fully offline on the local machine. "
        "Answer directly and never claim to run shell commands or access the OS yourself."
    )

    def __init__(
        self,
        backend_factory: Callable[[Path, ModelProfile], CompletionBackend] | None = None,
        *,
        isolate_model_process: bool | None = None,
        process_start_method: str | None = None,
        max_consecutive_failures: int = 3,
    ) -> None:
        self._model_backend_factory = backend_factory or LlamaCppBackend
        self._isolate_model_process = (
            isolate_model_process if isolate_model_process is not None else backend_factory is None
        )
        self._process_start_method = process_start_method
        self._max_consecutive_failures = max(1, max_consecutive_failures)
        self._consecutive_failures = 0
        self._backend: CompletionBackend | None = None
        self._loaded_profile: ModelProfile | None = None
        self._root: Path | None = None
        self._telemetry = LocalRuntimeTelemetry(loaded=False, backend="llama.cpp", device="cpu")

    def initialize(self, profile: ModelProfile, root: Path) -> None:
        self.validate(profile, root)
        model_path = resolve_model_path(profile, root=root)
        if not model_path.exists():
            self._record_error("missing_model", f"model file does not exist: {model_path}")
            self._set_process_state(ModelProcessState.CRASHED)
            raise MissingModelError(f"model file does not exist: {model_path}")
        if not model_path.is_file():
            self._record_error("invalid_model_path", f"model path is not a file: {model_path}")
            self._set_process_state(ModelProcessState.CRASHED)
            raise InvalidModelPathError(f"model path is not a file: {model_path}")
        if (
            self._backend is not None
            and self._loaded_profile == profile
            and self._telemetry.model_path == str(model_path)
            and self._telemetry.process_state == ModelProcessState.READY.value
            and self._backend_is_healthy()
        ):
            return

        if self._consecutive_failures >= self._max_consecutive_failures:
            self._record_error(
                "repeated_crash_protection",
                "local model runtime exceeded the allowed consecutive crash limit",
            )
            self._set_process_state(ModelProcessState.CRASHED)
            raise ModelLoadError(
                "local model runtime temporarily disabled after repeated crashes"
            )

        # Phase 5: check system resources before attempting to load.
        self._check_resources(profile)

        self.shutdown()
        self._set_process_state(ModelProcessState.LOADING)
        started = self._snapshot()
        started_at = time.perf_counter()
        try:
            self._backend = self._create_backend(model_path, profile)
        except LocalRuntimeError as exc:
            self._handle_runtime_failure(self._error_kind_for_exception(exc), str(exc))
            raise
        except MemoryError as exc:
            wrapped = InsufficientMemoryError("insufficient memory while loading the local Qwen model")
            self._handle_runtime_failure("insufficient_memory", str(wrapped))
            raise wrapped from exc
        except Exception as exc:
            wrapped = ModelLoadError(f"failed to load local model from {model_path}")
            self._handle_runtime_failure("model_loading_failure", str(wrapped))
            raise wrapped from exc

        self._loaded_profile = profile
        self._root = root
        self._telemetry.loaded = True
        self._telemetry.backend = profile.runtime.engine
        self._telemetry.device = profile.runtime.device
        self._telemetry.model_path = str(model_path)
        self._telemetry.load_duration_ms = (time.perf_counter() - started_at) * 1000.0
        self._update_metrics(started)
        self._telemetry.model_pid = self._backend_pid()
        self._clear_error()
        self._consecutive_failures = 0
        self._set_process_state(ModelProcessState.READY)

    def shutdown(self) -> None:
        if self._backend is not None:
            self._backend.close()
        self._backend = None
        self._loaded_profile = None
        self._root = None
        self._telemetry.loaded = False
        self._telemetry.model_pid = None
        self._set_process_state(ModelProcessState.UNLOADED)

    def restart(self, profile: ModelProfile, root: Path) -> None:
        """Shut down the current backend (if any) and re-initialize.

        Useful for recovering from a crashed/hung runtime without restarting
        the whole Python agent or TuwaiqOS.  The broker is never touched
        during this operation.
        """
        self.shutdown()
        self.initialize(profile, root)

    def telemetry(self) -> dict[str, Any]:
        return self._telemetry.to_dict()

    def decide(self, user_message: str, profile: ModelProfile) -> Any | None:
        if not user_message.strip():
            return None
        prompt = self._build_tool_call_prompt(user_message)
        text = self._complete(prompt, profile)
        from model_provider import AgentAction
        from tool_call_parser import ToolCallError, is_shell_command_attempt, is_tool_call, parse_tool_call

        # If the model emitted a shell command string instead of JSON or NL,
        # discard it entirely and delegate to the fallback -- never echo it.
        if is_shell_command_attempt(text):
            return None

        if is_tool_call(text):
            try:
                parsed = parse_tool_call(text)
                return AgentAction(kind="call_tool", tool=parsed.tool, arguments=parsed.arguments)
            except ToolCallError:
                pass  # malformed/invalid tool call; fall through to plain response
        return AgentAction(kind="respond", text=text)

    def decide_next(
        self,
        user_message: str,
        accumulated: "list[ToolResultEntry]",
        context: "ConversationContext",
        profile: ModelProfile,
    ) -> Any | None:
        """Multi-step decision: build a prompt that includes already-collected
        tool results so the model can decide whether to call another tool or
        produce a final answer."""
        if not user_message.strip():
            return None
        prompt = self._build_multi_step_prompt(user_message, accumulated, context)
        text = self._complete(prompt, profile)
        from model_provider import AgentAction
        from tool_call_parser import ToolCallError, is_shell_command_attempt, is_tool_call, parse_tool_call

        if is_shell_command_attempt(text):
            return None

        if is_tool_call(text):
            try:
                parsed = parse_tool_call(text)
                return AgentAction(kind="call_tool", tool=parsed.tool, arguments=parsed.arguments)
            except ToolCallError:
                pass
        return AgentAction(kind="respond", text=text)

    def synthesize(
        self,
        user_message: str,
        accumulated: "list[ToolResultEntry]",
        context: "ConversationContext",
        profile: ModelProfile,
    ) -> str | None:
        """Build a synthesis prompt with all accumulated results and return
        the model's combined natural-language answer."""
        if not accumulated:
            return None
        prompt = self._build_synthesis_prompt(user_message, accumulated, context)
        return self._complete(prompt, profile)

    def explain(self, user_message: str, tool: str, result: dict[str, Any], profile: ModelProfile) -> str | None:
        prompt = self._build_tool_result_prompt(user_message, tool, result)
        return self._complete(prompt, profile)

    def explain_error(
        self,
        user_message: str,
        tool: str,
        error_code: str,
        error_message: str,
        profile: ModelProfile,
    ) -> str | None:
        prompt = self._build_tool_error_prompt(user_message, tool, error_code, error_message)
        return self._complete(prompt, profile)

    def validate(self, profile: ModelProfile, root: Path) -> None:
        resolve_model_path(profile, root=root)
        if profile.runtime.engine not in self._SUPPORTED_ENGINES:
            raise IncompatibleRuntimeError(
                f"runtime engine '{profile.runtime.engine}' is not compatible with GGUF Qwen profiles"
            )
        model_path = Path(profile.model_path)
        if model_path.suffix.lower() != ".gguf":
            raise InvalidModelPathError("Qwen local runtime expects a .gguf model file")

    def _complete(self, prompt: str, profile: ModelProfile) -> str:
        root = self._root or Path(__file__).resolve().parent.parent
        self.initialize(profile, root=root)
        if self._backend is None:
            raise ModelLoadError("local Qwen runtime is not initialized")

        started = self._snapshot()
        started_at = time.perf_counter()
        executor = ThreadPoolExecutor(max_workers=1)
        future = executor.submit(
            self._backend.generate,
            prompt,
            max_tokens=profile.context.max_output_tokens,
            temperature=profile.generation.temperature,
            top_p=profile.generation.top_p,
            top_k=profile.generation.top_k,
            stop=self._STOP_TOKENS,
        )
        try:
            text = future.result(timeout=profile.runtime.timeout_seconds)
        except FutureTimeoutError as exc:
            self._telemetry.inference_latency_ms = (time.perf_counter() - started_at) * 1000.0
            self._update_metrics(started)
            self._handle_runtime_failure("timeout", "local Qwen inference timed out")
            raise InferenceTimeoutError("local Qwen inference timed out") from exc
        except LocalRuntimeError as exc:
            self._telemetry.inference_latency_ms = (time.perf_counter() - started_at) * 1000.0
            self._update_metrics(started)
            self._handle_runtime_failure(self._error_kind_for_exception(exc), str(exc))
            raise
        except MemoryError as exc:
            self._telemetry.inference_latency_ms = (time.perf_counter() - started_at) * 1000.0
            self._update_metrics(started)
            wrapped = InsufficientMemoryError("OOM during inference")
            self._handle_runtime_failure("oom", str(wrapped))
            raise wrapped from exc
        except Exception as exc:
            self._telemetry.inference_latency_ms = (time.perf_counter() - started_at) * 1000.0
            self._update_metrics(started)
            wrapped = InferenceError("local Qwen inference failed")
            self._handle_runtime_failure("inference_failure", str(wrapped))
            raise wrapped from exc
        finally:
            executor.shutdown(wait=False, cancel_futures=True)

        self._telemetry.inference_latency_ms = (time.perf_counter() - started_at) * 1000.0
        self._update_metrics(started)
        self._telemetry.model_pid = self._backend_pid()
        self._clear_error()
        self._consecutive_failures = 0
        # Restore READY state after successful inference (may have been
        # CRASHED from a previous failed inference attempt that was recovered).
        if self._telemetry.loaded:
            self._set_process_state(ModelProcessState.READY)
        return text

    def _build_prompt(self, user_message: str) -> str:
        return (
            f"{self._SYSTEM_PROMPT}\n\n"
            f"User: {user_message.strip()}\n"
            "Assistant:"
        )

    def _build_tool_call_prompt(self, user_message: str) -> str:
        import json

        from tool_schemas import TOOL_SCHEMAS

        tools_json = json.dumps(TOOL_SCHEMAS, indent=2)
        return (
            f"{self._SYSTEM_PROMPT}\n\n"
            "You have access to the following tools. If the user's request requires\n"
            "a tool, respond with ONLY a single JSON object (no markdown, no extra text):\n"
            '{"tool": "<tool_name>", "arguments": {<args>}}\n\n'
            "If no tool is needed, reply naturally in plain text.\n\n"
            f"Available tools:\n{tools_json}\n\n"
            f"User: {user_message.strip()}\n"
            "Assistant:"
        )

    def _build_multi_step_prompt(
        self,
        user_message: str,
        accumulated: "list[ToolResultEntry]",
        context: "ConversationContext",
    ) -> str:
        """Prompt for the nth iteration of the agent loop.

        Includes a compact summary of already-collected tool results so the
        model can decide whether to call another tool or answer now.  Raw
        telemetry is never dumped verbatim -- only compact summaries.
        """
        import json

        from tool_schemas import TOOL_SCHEMAS

        tools_json = json.dumps(TOOL_SCHEMAS, indent=2)

        ctx_prefix = context.build_context_prefix()
        ctx_section = f"{ctx_prefix}\n\n" if ctx_prefix else ""

        if accumulated:
            collected_lines = ["Already collected:"]
            for entry in accumulated:
                status = "ok" if entry.ok else "error"
                summary = entry.summary or f"{entry.tool}: {status}"
                collected_lines.append(f"  - {summary}")
            collected_section = "\n".join(collected_lines) + "\n\n"
        else:
            collected_section = ""

        return (
            f"{self._SYSTEM_PROMPT}\n\n"
            f"{ctx_section}"
            f"{collected_section}"
            "You have access to the following tools.\n"
            "If you need more information, respond with ONLY a single JSON tool call:\n"
            '{"tool": "<tool_name>", "arguments": {<args>}}\n'
            "If you have enough information to answer the user, reply in plain text.\n\n"
            f"Available tools:\n{tools_json}\n\n"
            f"User: {user_message.strip()}\n"
            "Assistant:"
        )

    def _build_synthesis_prompt(
        self,
        user_message: str,
        accumulated: "list[ToolResultEntry]",
        context: "ConversationContext",
    ) -> str:
        """Prompt asking the model to synthesize a final answer from all results."""
        ctx_prefix = context.build_context_prefix()
        ctx_section = f"{ctx_prefix}\n\n" if ctx_prefix else ""

        results_lines: list[str] = []
        for entry in accumulated:
            if entry.ok and entry.result:
                results_lines.append(f"  - {entry.tool}: {entry.summary or str(entry.result)[:200]}")
            elif not entry.ok:
                results_lines.append(f"  - {entry.tool}: error")

        results_section = "Collected data:\n" + "\n".join(results_lines) if results_lines else ""

        return (
            f"{self._SYSTEM_PROMPT}\n\n"
            f"{ctx_section}"
            f"{results_section}\n\n"
            "Using ONLY the data above (do not invent any values), give a clear,\n"
            "concise natural-language answer to the user's request.\n"
            "Distinguish observed data from possible cause and conclusion.\n\n"
            f"User request: {user_message.strip()}\n"
            "Assistant:"
        )

    def _build_tool_result_prompt(self, user_message: str, tool: str, result: dict[str, Any]) -> str:
        return (
            f"{self._SYSTEM_PROMPT}\n\n"
            "Summarize the tool output for the user in one short answer.\n"
            f"User request: {user_message.strip()}\n"
            f"Tool: {tool}\n"
            f"Tool result: {result}\n"
            "Assistant:"
        )

    def _build_tool_error_prompt(
        self,
        user_message: str,
        tool: str,
        error_code: str,
        error_message: str,
    ) -> str:
        return (
            f"{self._SYSTEM_PROMPT}\n\n"
            "Explain the tool failure honestly without exposing internals.\n"
            f"User request: {user_message.strip()}\n"
            f"Tool: {tool}\n"
            f"Error code: {error_code}\n"
            f"Error message: {error_message}\n"
            "Assistant:"
        )

    def _snapshot(self) -> tuple[float, float]:
        return time.perf_counter(), self._cpu_seconds()

    def _update_metrics(self, started: tuple[float, float]) -> None:
        started_at, started_cpu = started
        wall_delta = max(time.perf_counter() - started_at, 1e-6)
        cpu_delta = max(self._cpu_seconds() - started_cpu, 0.0)
        self._telemetry.ram_usage_mb = self._ram_usage_mb()
        self._telemetry.cpu_usage_percent = (cpu_delta / wall_delta) * 100.0
        self._telemetry.vram_usage_mb = None
        self._telemetry.gpu_usage_percent = None

    def _record_error(self, kind: str, message: str) -> None:
        self._telemetry.last_error_kind = kind
        self._telemetry.last_error_message = message

    def _clear_error(self) -> None:
        self._telemetry.last_error_kind = None
        self._telemetry.last_error_message = None

    def _set_process_state(self, state: ModelProcessState) -> None:
        self._telemetry.process_state = state.value

    def _check_resources(self, profile: ModelProfile) -> None:
        """Pre-flight RAM/VRAM check.  Raises InsufficientRAMError /
        InsufficientVRAMError when the system clearly cannot satisfy the
        profile's minimum requirements.  Silently passes when measurement
        is unavailable so as not to false-positive on CI or unusual envs.
        """
        from resource_manager import check_ram_for_profile, check_vram_for_profile

        ram_ok, ram_msg = check_ram_for_profile(profile.hardware.min_ram_gb)
        if not ram_ok:
            self._record_error("insufficient_ram", ram_msg)
            self._set_process_state(ModelProcessState.CRASHED)
            raise InsufficientRAMError(ram_msg)

        if profile.runtime.gpu_layers > 0:
            vram_ok, vram_msg = check_vram_for_profile(profile.hardware.min_vram_gb)
            if not vram_ok:
                self._record_error("insufficient_vram", vram_msg)
                self._set_process_state(ModelProcessState.CRASHED)
                raise InsufficientVRAMError(vram_msg)

    def _cpu_seconds(self) -> float:
        backend = self._backend
        getter = getattr(backend, "cpu_seconds", None) if backend is not None else None
        if callable(getter):
            try:
                return float(getter())
            except Exception:
                pass
        usage = resource.getrusage(resource.RUSAGE_SELF)
        return usage.ru_utime + usage.ru_stime

    def _ram_usage_mb(self) -> float:
        backend = self._backend
        getter = getattr(backend, "ram_usage_mb", None) if backend is not None else None
        if callable(getter):
            try:
                value = getter()
            except Exception:
                value = None
            if value is not None:
                return float(value)
        usage = resource.getrusage(resource.RUSAGE_SELF)
        rss = float(usage.ru_maxrss)
        if sys.platform == "darwin":
            return rss / (1024.0 * 1024.0)
        return rss / 1024.0

    def _error_kind_for_exception(self, exc: LocalRuntimeError) -> str:
        if isinstance(exc, MissingModelError):
            return "missing_model"
        if isinstance(exc, InvalidModelPathError):
            return "invalid_model_path"
        if isinstance(exc, IncompatibleRuntimeError):
            return "incompatible_runtime"
        if isinstance(exc, InsufficientRAMError):
            return "insufficient_ram"
        if isinstance(exc, InsufficientVRAMError):
            return "insufficient_vram"
        if isinstance(exc, InsufficientMemoryError):
            return "insufficient_memory"
        if isinstance(exc, InferenceTimeoutError):
            return "timeout"
        if isinstance(exc, InvalidRuntimeResponseError):
            return "invalid_response"
        if isinstance(exc, BrokenIPCError):
            return "broken_ipc"
        if isinstance(exc, ModelLoadError):
            return "model_loading_failure"
        if isinstance(exc, InferenceError):
            return "inference_failure"
        return "runtime_error"

    def _create_backend(self, model_path: Path, profile: ModelProfile) -> CompletionBackend:
        if self._isolate_model_process:
            return IsolatedCompletionBackend(
                model_path,
                profile,
                backend_factory=self._model_backend_factory,
                start_method=self._process_start_method,
            )
        return self._model_backend_factory(model_path, profile)

    def _handle_runtime_failure(self, kind: str, message: str) -> None:
        self._consecutive_failures += 1
        self._record_error(kind, message)
        self._telemetry.loaded = False
        self._telemetry.model_pid = None
        self._set_process_state(ModelProcessState.CRASHED)
        if self._backend is not None:
            try:
                self._backend.close()
            except Exception:
                pass
        self._backend = None
        self._loaded_profile = None
        self._root = None

    def _backend_is_healthy(self) -> bool:
        backend = self._backend
        if backend is None:
            return False
        health_check = getattr(backend, "health_check", None)
        if callable(health_check):
            try:
                return bool(health_check())
            except LocalRuntimeError:
                return False
        return True

    def _backend_pid(self) -> int | None:
        backend = self._backend
        if backend is None:
            return None
        getter = getattr(backend, "pid", None)
        if callable(getter):
            try:
                pid = getter()
            except Exception:
                return None
            return int(pid) if pid is not None else None
        return None
