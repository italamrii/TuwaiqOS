"""Product CLI client for the Tuwaiq AI local socket API.

Talks ONLY to the service socket. Never creates a parallel agent/provider path.
Commands: status | chat | provision-model
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
from typing import Any

from config import AgentConfig
from runtime_status import (
    DEFAULT_API_SOCKET,
    PRODUCT_MODEL_ID,
    collect_status,
)

MAX_LINE_BYTES = 64 * 1024


class SocketClient:
    def __init__(self, path: str | None = None):
        self.path = path or os.environ.get("TUWAIQ_AI_SOCKET", DEFAULT_API_SOCKET)

    def call(self, method: str, params: dict[str, Any] | None = None, timeout: float = 120.0) -> dict[str, Any]:
        request = {"method": method, "params": params or {}}
        raw = (json.dumps(request, ensure_ascii=False) + "\n").encode("utf-8")
        if len(raw) > MAX_LINE_BYTES:
            raise RuntimeError("request too large")
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(timeout)
        try:
            sock.connect(self.path)
            sock.sendall(raw)
            with sock.makefile("rb") as stream:
                line = stream.readline(MAX_LINE_BYTES + 1)
        finally:
            sock.close()
        if not line:
            raise RuntimeError("empty response from tuwaiq-ai service")
        if len(line) > MAX_LINE_BYTES:
            raise RuntimeError("response too large")
        return json.loads(line.decode("utf-8"))


def cmd_status(args: argparse.Namespace) -> int:
    client = SocketClient(args.socket)
    try:
        response = client.call("get_status", timeout=10.0)
        if not response.get("ok"):
            err = response.get("error") or {}
            print(f"service error: {err.get('code')}: {err.get('message')}", file=sys.stderr)
            # Fall back to local factual probe so MODEL_NOT_INSTALLED is still visible.
            status = collect_status(AgentConfig.from_environ(), service_ready=False)
            print(json.dumps(status.to_dict(), indent=2, ensure_ascii=False))
            return 1
        print(json.dumps(response.get("result") or {}, indent=2, ensure_ascii=False))
        state = (response.get("result") or {}).get("state")
        return 0 if state in {"READY", "MODEL_READY", "MODEL_LOADING", "MODEL_NOT_INSTALLED"} else 1
    except (OSError, RuntimeError, json.JSONDecodeError) as exc:
        print(f"tuwaiq-ai service unreachable ({exc})", file=sys.stderr)
        status = collect_status(AgentConfig.from_environ(), broker_ok=False, service_ready=False)
        # Prefer MODEL_NOT_INSTALLED when that is the factual blocker for Product ISO.
        payload = status.to_dict()
        payload["service_reachable"] = False
        payload["service_error"] = str(exc)
        print(json.dumps(payload, indent=2, ensure_ascii=False))
        return 1


def cmd_chat(args: argparse.Namespace) -> int:
    client = SocketClient(args.socket)
    created = client.call("create_session")
    if not created.get("ok"):
        err = created.get("error") or {}
        print(f"create_session failed: {err.get('code')}: {err.get('message')}", file=sys.stderr)
        return 1
    session_id = (created.get("result") or {})["session_id"]
    print(
        "Tuwaiq AI (service client) — type 'exit' to quit.\n"
        f"session={session_id}\n"
        "Future GUI panels must use this same socket API only.\n"
    )
    try:
        while True:
            try:
                user_input = input("You: ").strip()
            except (EOFError, KeyboardInterrupt):
                print()
                break
            if not user_input:
                continue
            if user_input.lower() in {"exit", "quit"}:
                break
            response = client.call(
                "send_message",
                {"session_id": session_id, "message": user_input},
            )
            if not response.get("ok"):
                err = response.get("error") or {}
                print(f"error: {err.get('code')}: {err.get('message')}\n")
                continue
            result = response.get("result") or {}
            state = result.get("state")
            reply = result.get("reply") or ""
            if state == "ACTION_CONFIRMATION_REQUIRED":
                print(f"Tuwaiq [{state}]: {reply}\n")
            else:
                print(f"Tuwaiq: {reply}\n")
    finally:
        try:
            client.call("close_session", {"session_id": session_id}, timeout=5.0)
        except Exception:
            pass
    print("Goodbye.")
    return 0


def cmd_provision_model(args: argparse.Namespace) -> int:
    """Explicit user-invoked model install. Never runs at boot."""
    config = AgentConfig.from_environ()
    model = args.model or config.model_id or PRODUCT_MODEL_ID
    ollama = shutil.which("ollama")
    if not ollama:
        print(
            "Ollama runtime is not installed on this system.\n"
            "Install the official Ollama package for your platform, then re-run:\n"
            f"  tuwaiq-ai provision-model --model {model}\n"
            "The Developer ISO does not embed Ollama or model weights.",
            file=sys.stderr,
        )
        return 2

    print(f"Pulling model via local Ollama: {model}")
    print("This is an explicit user action; boot never performs this pull.")
    try:
        proc = subprocess.run(  # noqa: S603 — fixed argv, no shell
            [ollama, "pull", model],
            check=False,
        )
    except OSError as exc:
        print(f"failed to invoke ollama: {exc}", file=sys.stderr)
        return 1
    if proc.returncode != 0:
        print(f"ollama pull failed with exit {proc.returncode}", file=sys.stderr)
        return proc.returncode
    print(f"Model provisioned: {model}")
    print("Next: systemctl restart tuwaiq-ai.service  # if the service was already running")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="tuwaiq-ai",
        description="Tuwaiq AI Product CLI (local socket client)",
    )
    parser.add_argument(
        "--socket",
        default=os.environ.get("TUWAIQ_AI_SOCKET", DEFAULT_API_SOCKET),
        help="Unix socket path for tuwaiq-ai.service",
    )
    sub = parser.add_subparsers(dest="command", required=True)

    p_status = sub.add_parser("status", help="Show factual service/model status")
    p_status.set_defaults(func=cmd_status)

    p_chat = sub.add_parser("chat", help="Interactive chat via the service socket")
    p_chat.set_defaults(func=cmd_chat)

    p_prov = sub.add_parser(
        "provision-model",
        help="Explicitly pull the local model via Ollama (never at boot)",
    )
    p_prov.add_argument(
        "--model",
        default=PRODUCT_MODEL_ID,
        help=f"Ollama model tag (default: {PRODUCT_MODEL_ID})",
    )
    p_prov.set_defaults(func=cmd_provision_model)
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    return int(args.func(args) or 0)


if __name__ == "__main__":
    sys.exit(main())
