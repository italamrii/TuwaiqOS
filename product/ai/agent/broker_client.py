"""Manages the tuwaiq-agent-broker connection.

Two modes:
1. Subprocess stdin/stdout (dev/tests) — default when no socket env is set.
2. Unix-domain socket to a systemd-managed broker (Product) when
   TUWAIQ_AI_BROKER_SOCKET is set.

This is the *only* file in the Python codebase that spawns a process or
opens the broker transport. `agent.py` never sees a subprocess handle.
"""

from __future__ import annotations

import json
import logging
import os
import socket
import subprocess
import time
from pathlib import Path

from protocol import ToolRequest, ToolResponse

logger = logging.getLogger("tuwaiq_agent.broker_client")

DEFAULT_BROKER_PATH = Path(__file__).resolve().parent.parent / "broker" / "target" / "debug" / "tuwaiq-agent-broker"
CALL_TIMEOUT_SECONDS = 30.0
MAX_RESTART_ATTEMPTS = 3


def _normalize_broker_path(path: Path) -> Path:
    """Prefer a platform-native broker binary name when present."""
    if path.exists():
        return path
    exe = path.with_suffix(".exe")
    if exe.exists():
        return exe
    return path


class BrokerUnavailableError(RuntimeError):
    """Raised when the broker cannot be started or kept alive after
    MAX_RESTART_ATTEMPTS — the agent should surface this to the user as
    "system tools are temporarily unavailable," not crash itself."""


class BrokerClient:
    def __init__(self, broker_path: Path | str = DEFAULT_BROKER_PATH):
        self._broker_path = _normalize_broker_path(Path(broker_path))
        self._proc: subprocess.Popen | None = None
        self._restart_count = 0
        self._socket_path = os.environ.get("TUWAIQ_AI_BROKER_SOCKET") or None
        self._sock: socket.socket | None = None
        self._sock_file = None

    @property
    def uses_socket(self) -> bool:
        return bool(self._socket_path)

    def _is_alive(self) -> bool:
        if self._socket_path:
            return self._sock is not None
        return self._proc is not None and self._proc.poll() is None

    def _start(self) -> None:
        if self._socket_path:
            self._connect_socket()
            return
        if not self._broker_path.exists():
            raise BrokerUnavailableError(
                f"broker binary not found at {self._broker_path}; run `cargo build` in broker/"
            )
        logger.info("starting broker subprocess: %s", self._broker_path)
        if self._broker_path.suffix.lower() in {".cmd", ".bat"}:
            argv = ["cmd.exe", "/c", str(self._broker_path)]
        else:
            argv = [str(self._broker_path)]
        self._proc = subprocess.Popen(
            argv,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )

    def _connect_socket(self) -> None:
        assert self._socket_path is not None
        path = Path(self._socket_path)
        if not path.exists():
            raise BrokerUnavailableError(
                f"broker socket not found at {path}; is tuwaiq-agent-broker.service running?"
            )
        logger.info("connecting to broker socket: %s", path)
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(CALL_TIMEOUT_SECONDS)
        try:
            sock.connect(str(path))
        except OSError as exc:
            sock.close()
            raise BrokerUnavailableError(f"broker socket connect failed: {exc}") from exc
        self._sock = sock
        self._sock_file = sock.makefile("rwb", buffering=0)

    def _ensure_alive(self) -> None:
        if self._is_alive():
            return
        if self._socket_path:
            if self._restart_count >= MAX_RESTART_ATTEMPTS:
                raise BrokerUnavailableError(
                    f"broker socket failed after {MAX_RESTART_ATTEMPTS} reconnect attempts"
                )
            self._restart_count += 1
            self._close_socket()
            self._connect_socket()
            return
        if self._proc is not None:
            logger.warning(
                "broker process is not running (exit code=%s); restarting",
                self._proc.poll(),
            )
        if self._restart_count >= MAX_RESTART_ATTEMPTS:
            raise BrokerUnavailableError(
                f"broker failed to stay alive after {MAX_RESTART_ATTEMPTS} restart attempts"
            )
        self._restart_count += 1
        self._start()

    def call(self, tool: str, arguments: dict | None = None) -> ToolResponse:
        """Send one tool request and block for the matching response."""
        request = ToolRequest(tool=tool, arguments=arguments or {})
        self._ensure_alive()
        line = json.dumps(request.to_wire_dict())

        if self._socket_path:
            return self._call_socket(request, line)

        assert self._proc is not None and self._proc.stdin is not None and self._proc.stdout is not None
        try:
            self._proc.stdin.write(line + "\n")
            self._proc.stdin.flush()
        except (BrokenPipeError, OSError) as e:
            logger.warning("broker pipe broken on write (%s); restarting and retrying once", e)
            self._proc = None
            self._ensure_alive()
            assert self._proc is not None and self._proc.stdin is not None
            self._proc.stdin.write(line + "\n")
            self._proc.stdin.flush()

        response_line = self._read_response_line(request.request_id)
        if response_line is None:
            return ToolResponse(
                protocol_version=request.protocol_version,
                request_id=request.request_id,
                timestamp=request.timestamp,
                status="error",
                error={
                    "code": "internal_error",
                    "message": "broker process did not respond (it may have crashed); it will be restarted on the next request",
                },
            )

        try:
            data = json.loads(response_line)
        except json.JSONDecodeError:
            return ToolResponse(
                protocol_version=request.protocol_version,
                request_id=request.request_id,
                timestamp=request.timestamp,
                status="error",
                error={"code": "internal_error", "message": "broker returned unparseable output"},
            )
        return ToolResponse.from_wire_dict(data)

    def _call_socket(self, request: ToolRequest, line: str) -> ToolResponse:
        assert self._sock_file is not None
        try:
            self._sock_file.write((line + "\n").encode("utf-8"))
            self._sock_file.flush()
            raw = self._sock_file.readline()
        except OSError as exc:
            logger.warning("broker socket I/O failed (%s); reconnecting once", exc)
            self._close_socket()
            self._ensure_alive()
            assert self._sock_file is not None
            self._sock_file.write((line + "\n").encode("utf-8"))
            self._sock_file.flush()
            raw = self._sock_file.readline()

        if not raw:
            self._close_socket()
            return ToolResponse(
                protocol_version=request.protocol_version,
                request_id=request.request_id,
                timestamp=request.timestamp,
                status="error",
                error={
                    "code": "internal_error",
                    "message": "broker socket closed without a response",
                },
            )
        try:
            data = json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            return ToolResponse(
                protocol_version=request.protocol_version,
                request_id=request.request_id,
                timestamp=request.timestamp,
                status="error",
                error={"code": "internal_error", "message": "broker returned unparseable output"},
            )
        return ToolResponse.from_wire_dict(data)

    def _read_response_line(self, expected_request_id: str) -> str | None:
        assert self._proc is not None and self._proc.stdout is not None
        deadline = time.monotonic() + CALL_TIMEOUT_SECONDS
        while time.monotonic() < deadline:
            if not self._is_alive():
                return None
            line = self._proc.stdout.readline()
            if not line:
                return None
            line = line.strip()
            if not line:
                continue
            return line
        logger.warning("timed out waiting for broker response to request_id=%s", expected_request_id)
        return None

    def _close_socket(self) -> None:
        if self._sock_file is not None:
            try:
                self._sock_file.close()
            except Exception:
                pass
        self._sock_file = None
        if self._sock is not None:
            try:
                self._sock.close()
            except Exception:
                pass
        self._sock = None

    def shutdown(self) -> None:
        if self._socket_path:
            self._close_socket()
            return
        if self._proc is not None and self._is_alive():
            try:
                self._proc.stdin.close()  # type: ignore[union-attr]
            except Exception:
                pass
            self._proc.terminate()
            try:
                self._proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self._proc.kill()
        self._proc = None
