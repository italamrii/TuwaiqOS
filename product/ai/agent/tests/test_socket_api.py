"""Socket API unit tests (in-process dispatch; no live Ollama).

Product uses AF_UNIX on Linux. Host Windows Python may lack AF_UNIX, so these
tests exercise the method/schema surface via AgentSocketServer.dispatch().
"""

from __future__ import annotations

from config import AgentConfig
from model_provider import RuleBasedProvider
from socket_api import AgentSocketServer
from tests.conftest import FakeBroker


def _server() -> AgentSocketServer:
    config = AgentConfig(provider="rule", model_id="rule-test")
    broker = FakeBroker()
    server = AgentSocketServer(
        socket_path="/tmp/tuwaiq-ai-test.sock",
        config=config,
        broker=broker,  # type: ignore[arg-type]
    )
    server._model = RuleBasedProvider()  # noqa: SLF001 — test seam
    return server


def test_unknown_method_rejected():
    resp = _server().dispatch({"method": "shell_exec", "params": {"cmd": "id"}})
    assert resp["ok"] is False
    assert resp["error"]["code"] == "unknown_method"


def test_create_send_close_session():
    server = _server()
    created = server.dispatch({"method": "create_session", "params": {}})
    assert created["ok"] is True
    sid = created["result"]["session_id"]
    sent = server.dispatch(
        {
            "method": "send_message",
            "params": {"session_id": sid, "message": "hello"},
        }
    )
    assert sent["ok"] is True
    assert sent["result"]["state"] == "FINAL_RESPONSE"
    assert "reply" in sent["result"]
    closed = server.dispatch(
        {"method": "close_session", "params": {"session_id": sid}}
    )
    assert closed["ok"] is True


def test_confirmation_state():
    server = _server()
    sid = server.dispatch({"method": "create_session", "params": {}})["result"][
        "session_id"
    ]
    sent = server.dispatch(
        {
            "method": "send_message",
            "params": {
                "session_id": sid,
                "message": "terminate process firefox",
            },
        }
    )
    assert sent["ok"] is True
    assert sent["result"]["state"] == "ACTION_CONFIRMATION_REQUIRED"
    cancel = server.dispatch(
        {"method": "cancel_action", "params": {"session_id": sid}}
    )
    assert cancel["ok"] is True
    assert cancel["result"]["state"] == "FINAL_RESPONSE"


def test_get_status_rule_provider():
    server = _server()
    status = server.dispatch({"method": "get_status", "params": {}})
    assert status["ok"] is True
    assert "state" in status["result"]
    assert status["result"]["provider"] == "rule"


def test_malformed_request_object():
    resp = _server().dispatch(["not", "an", "object"])
    assert resp["ok"] is False
    assert resp["error"]["code"] == "invalid_request"
