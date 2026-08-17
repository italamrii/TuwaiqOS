"""Local Unix-domain socket API for the Product Tuwaiq AI service.

Newline-delimited JSON request/response. Local-only. Strict method allowlist,
schema, size, and timeout checks. Sessions map to in-memory Agent instances
with no default history persistence on disk.
"""

from __future__ import annotations

import json
import logging
import os
import socket
import threading
import time
import uuid
from dataclasses import dataclass, field
from typing import Any, Callable

from agent import Agent
from broker_client import BrokerClient
from config import AgentConfig
from model_provider import build_provider
from runtime_status import DEFAULT_API_SOCKET, collect_status

logger = logging.getLogger("tuwaiq_agent.socket_api")

ALLOWED_METHODS = frozenset(
    {
        "create_session",
        "send_message",
        "confirm_action",
        "cancel_action",
        "get_status",
        "close_session",
    }
)

MAX_LINE_BYTES = 64 * 1024
MAX_SESSIONS = 8
DEFAULT_REQUEST_TIMEOUT = 120.0
CONFIRMATION_STATE = "ACTION_CONFIRMATION_REQUIRED"
FINAL_STATE = "FINAL_RESPONSE"


@dataclass
class SessionEntry:
    session_id: str
    agent: Agent
    created_at: float = field(default_factory=time.time)
    last_used: float = field(default_factory=time.time)


class AgentSocketServer:
    """Serve the Product local API on a Unix domain socket."""

    def __init__(
        self,
        socket_path: str | None = None,
        config: AgentConfig | None = None,
        broker: BrokerClient | None = None,
    ):
        self._config = config or AgentConfig.from_environ()
        self._socket_path = socket_path or os.environ.get(
            "TUWAIQ_AI_SOCKET", DEFAULT_API_SOCKET
        )
        self._broker = broker or BrokerClient(self._config.resolve_broker_path())
        self._model = build_provider(self._config)
        self._sessions: dict[str, SessionEntry] = {}
        self._lock = threading.RLock()
        self._server_sock: socket.socket | None = None
        self._stop = threading.Event()

    @property
    def socket_path(self) -> str:
        return self._socket_path

    def serve_forever(self) -> None:
        path = self._socket_path
        parent = os.path.dirname(path)
        if parent:
            os.makedirs(parent, mode=0o755, exist_ok=True)
        if os.path.exists(path):
            os.unlink(path)

        srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        srv.bind(path)
        os.chmod(path, 0o666)
        srv.listen(16)
        srv.settimeout(1.0)
        self._server_sock = srv
        logger.info("tuwaiq-ai socket API listening on %s", path)

        try:
            while not self._stop.is_set():
                try:
                    conn, _ = srv.accept()
                except socket.timeout:
                    continue
                except OSError:
                    if self._stop.is_set():
                        break
                    raise
                thread = threading.Thread(
                    target=self._handle_connection,
                    args=(conn,),
                    daemon=True,
                    name="tuwaiq-ai-client",
                )
                thread.start()
        finally:
            self.shutdown()

    def stop(self) -> None:
        self._stop.set()
        if self._server_sock is not None:
            try:
                self._server_sock.close()
            except OSError:
                pass

    def shutdown(self) -> None:
        self.stop()
        with self._lock:
            self._sessions.clear()
        try:
            self._broker.shutdown()
        except Exception:
            logger.exception("broker shutdown failed")
        if self._socket_path and os.path.exists(self._socket_path):
            try:
                os.unlink(self._socket_path)
            except OSError:
                pass

    def _handle_connection(self, conn: socket.socket) -> None:
        try:
            conn.settimeout(DEFAULT_REQUEST_TIMEOUT)
            with conn, conn.makefile("rwb", buffering=0) as stream:
                while not self._stop.is_set():
                    raw = stream.readline(MAX_LINE_BYTES + 1)
                    if not raw:
                        break
                    if len(raw) > MAX_LINE_BYTES:
                        self._write(
                            stream,
                            {
                                "ok": False,
                                "error": {
                                    "code": "payload_too_large",
                                    "message": f"request exceeds {MAX_LINE_BYTES} bytes",
                                },
                            },
                        )
                        break
                    try:
                        request = json.loads(raw.decode("utf-8"))
                    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
                        self._write(
                            stream,
                            {
                                "ok": False,
                                "error": {
                                    "code": "malformed_json",
                                    "message": str(exc),
                                },
                            },
                        )
                        continue
                    response = self.dispatch(request)
                    self._write(stream, response)
        except Exception:
            logger.exception("client connection failed")
        finally:
            try:
                conn.close()
            except OSError:
                pass

    @staticmethod
    def _write(stream: Any, payload: dict[str, Any]) -> None:
        line = json.dumps(payload, ensure_ascii=False) + "\n"
        stream.write(line.encode("utf-8"))
        stream.flush()

    def dispatch(self, request: Any) -> dict[str, Any]:
        if not isinstance(request, dict):
            return _err("invalid_request", "request must be a JSON object")
        method = request.get("method")
        if not isinstance(method, str) or method not in ALLOWED_METHODS:
            return _err("unknown_method", f"method must be one of {sorted(ALLOWED_METHODS)}")
        params = request.get("params") or {}
        if not isinstance(params, dict):
            return _err("invalid_params", "params must be an object")
        req_id = request.get("id")
        try:
            result = self._call(method, params)
        except ApiError as exc:
            body = _err(exc.code, exc.message)
            if req_id is not None:
                body["id"] = req_id
            return body
        except Exception as exc:  # noqa: BLE001
            logger.exception("API method %s failed", method)
            body = _err("internal_error", str(exc))
            if req_id is not None:
                body["id"] = req_id
            return body
        body = {"ok": True, "result": result}
        if req_id is not None:
            body["id"] = req_id
        return body

    def _call(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        handlers: dict[str, Callable[[dict[str, Any]], dict[str, Any]]] = {
            "create_session": self._create_session,
            "send_message": self._send_message,
            "confirm_action": self._confirm_action,
            "cancel_action": self._cancel_action,
            "get_status": self._get_status,
            "close_session": self._close_session,
        }
        return handlers[method](params)

    def _create_session(self, params: dict[str, Any]) -> dict[str, Any]:
        del params
        with self._lock:
            self._evict_if_needed()
            if len(self._sessions) >= MAX_SESSIONS:
                raise ApiError("session_limit", f"at most {MAX_SESSIONS} sessions allowed")
            session_id = str(uuid.uuid4())
            agent = Agent(
                model=self._model,
                broker=self._broker,
                config=self._config,
            )
            self._sessions[session_id] = SessionEntry(session_id=session_id, agent=agent)
        return {"session_id": session_id}

    def _send_message(self, params: dict[str, Any]) -> dict[str, Any]:
        session = self._require_session(params)
        message = params.get("message")
        if not isinstance(message, str) or not message.strip():
            raise ApiError("invalid_params", "message must be a non-empty string")
        if len(message.encode("utf-8")) > MAX_LINE_BYTES:
            raise ApiError("payload_too_large", "message too large")
        reply = session.agent.handle(message)
        return self._message_result(session, reply)

    def _confirm_action(self, params: dict[str, Any]) -> dict[str, Any]:
        session = self._require_session(params)
        if session.agent.memory.pending_confirmation is None:
            raise ApiError("no_pending_action", "no action awaiting confirmation")
        reply = session.agent.handle("yes")
        return self._message_result(session, reply)

    def _cancel_action(self, params: dict[str, Any]) -> dict[str, Any]:
        session = self._require_session(params)
        if session.agent.memory.pending_confirmation is None:
            raise ApiError("no_pending_action", "no action awaiting confirmation")
        reply = session.agent.handle("no")
        return self._message_result(session, reply)

    def _get_status(self, params: dict[str, Any]) -> dict[str, Any]:
        del params
        status = collect_status(self._config, service_ready=True)
        payload = status.to_dict()
        with self._lock:
            payload["sessions"] = len(self._sessions)
            payload["max_sessions"] = MAX_SESSIONS
        payload["socket"] = self._socket_path
        return payload

    def _close_session(self, params: dict[str, Any]) -> dict[str, Any]:
        session_id = params.get("session_id")
        if not isinstance(session_id, str) or not session_id:
            raise ApiError("invalid_params", "session_id is required")
        with self._lock:
            self._sessions.pop(session_id, None)
        return {"closed": True, "session_id": session_id}

    def _require_session(self, params: dict[str, Any]) -> SessionEntry:
        session_id = params.get("session_id")
        if not isinstance(session_id, str) or not session_id:
            raise ApiError("invalid_params", "session_id is required")
        with self._lock:
            entry = self._sessions.get(session_id)
            if entry is None:
                raise ApiError("unknown_session", f"session '{session_id}' not found")
            entry.last_used = time.time()
            return entry

    def _message_result(self, session: SessionEntry, reply: str) -> dict[str, Any]:
        pending = session.agent.memory.pending_confirmation
        state = CONFIRMATION_STATE if pending is not None else FINAL_STATE
        result: dict[str, Any] = {
            "session_id": session.session_id,
            "reply": reply,
            "state": state,
        }
        if pending is not None:
            result["pending_action"] = {
                "tool": pending.get("tool"),
                "summary": pending.get("summary"),
                "target": pending.get("target"),
                "risk": pending.get("risk"),
            }
        return result

    def _evict_if_needed(self) -> None:
        # Drop least-recently-used idle sessions when at capacity before create.
        if len(self._sessions) < MAX_SESSIONS:
            return
        oldest = min(self._sessions.values(), key=lambda s: s.last_used)
        self._sessions.pop(oldest.session_id, None)


class ApiError(Exception):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code
        self.message = message


def _err(code: str, message: str) -> dict[str, Any]:
    return {"ok": False, "error": {"code": code, "message": message}}
