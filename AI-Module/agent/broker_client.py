"""Manages the tuwaiq-agent-broker subprocess and the request/response
exchange over its stdin/stdout.

This is the *only* file in the Python codebase that spawns a process or
knows the broker's binary path. `agent.py` and `tools.py` never see a
subprocess handle -- they only ever call `BrokerClient.call(tool, args)` and
get back a `ToolResponse`.

Crash handling: if the broker process has died (crashed, was killed, or
exited), the next `call()` transparently restarts it before sending the
request. This directly satisfies requirement 15 ("if Python/AI crashes, the
broker and OS must remain usable") from the other direction that matters for
this prototype: even if the *broker* dies, the agent recovers on the next
call rather than wedging forever. In the real deployed system the broker
would be a systemd-supervised service the AI process does not own the
lifecycle of at all -- see architecture.md's "Process supervision" section
for why that matters for the reverse direction (Python/model crashing must
never be able to take the broker down, which is trivially true here since
Python never sends the broker anything but well-formed JSON on a pipe it
does not control the broker's exit with).
"""

from __future__ import annotations

import json
import logging
import subprocess
import time
from pathlib import Path

from protocol import ToolRequest, ToolResponse

logger = logging.getLogger("tuwaiq_agent.broker_client")

import platform

_BROKER_BINARY_NAME = "tuwaiq-agent-broker.exe" if platform.system() == "Windows" else "tuwaiq-agent-broker"
DEFAULT_BROKER_PATH = Path(__file__).resolve().parent.parent / "broker" / "target" / "debug" / _BROKER_BINARY_NAME
CALL_TIMEOUT_SECONDS = 10.0
MAX_RESTART_ATTEMPTS = 3


class BrokerUnavailableError(RuntimeError):
    """Raised when the broker cannot be started or kept alive after
    MAX_RESTART_ATTEMPTS -- the agent should surface this to the user as
    "system tools are temporarily unavailable," not crash itself."""


class BrokerClient:
    def __init__(self, broker_path: Path | str = DEFAULT_BROKER_PATH):
        self._broker_path = Path(broker_path)
        self._proc: subprocess.Popen | None = None
        self._restart_count = 0

    def _is_alive(self) -> bool:
        return self._proc is not None and self._proc.poll() is None

    def _start(self) -> None:
        if not self._broker_path.exists():
            raise BrokerUnavailableError(
                f"broker binary not found at {self._broker_path}; run `cargo build` in broker/"
            )
        logger.info("starting broker subprocess: %s", self._broker_path)
        self._proc = subprocess.Popen(
            [str(self._broker_path)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,  # line-buffered
        )

    def _ensure_alive(self) -> None:
        if self._is_alive():
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
        """Send one tool request and block for the matching response.

        Phase 1 is strictly synchronous/single-in-flight: the CLI prototype
        never has two requests outstanding at once, so a line-in/line-out
        exchange is sufficient and request_id correlation is trivial (there
        is only ever one candidate line to read). A concurrent multi-client
        broker is out of scope for Phase 1 -- see architecture.md.
        """
        request = ToolRequest(tool=tool, arguments=arguments or {})
        self._ensure_alive()
        assert self._proc is not None and self._proc.stdin is not None and self._proc.stdout is not None

        line = json.dumps(request.to_wire_dict())
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
            # The broker died mid-request (crashed) rather than answering.
            # Surface this as a structured error the agent can explain to
            # the user, rather than letting the caller hang or raising an
            # unhandled exception up through the agent loop.
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

    def _read_response_line(self, expected_request_id: str) -> str | None:
        assert self._proc is not None and self._proc.stdout is not None
        deadline = time.monotonic() + CALL_TIMEOUT_SECONDS
        while time.monotonic() < deadline:
            if not self._is_alive():
                return None
            line = self._proc.stdout.readline()
            if not line:
                # EOF on stdout -- broker exited.
                return None
            line = line.strip()
            if not line:
                continue
            return line
        logger.warning("timed out waiting for broker response to request_id=%s", expected_request_id)
        return None

    def shutdown(self) -> None:
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
