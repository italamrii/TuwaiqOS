"""Fake broker + helpers for agent unit/integration tests without a live broker."""

from __future__ import annotations

from typing import Any

from protocol import PROTOCOL_VERSION, ToolResponse


class FakeBroker:
    """In-process stand-in for BrokerClient used by Python tests."""

    def __init__(self, responses: dict[str, dict[str, Any]] | None = None):
        self.calls: list[tuple[str, dict[str, Any]]] = []
        self.responses = responses or {}
        self.fail_tools: set[str] = set()
        self.unavailable = False

    def call(self, tool: str, arguments: dict | None = None) -> ToolResponse:
        arguments = arguments or {}
        self.calls.append((tool, dict(arguments)))
        if self.unavailable:
            from broker_client import BrokerUnavailableError

            raise BrokerUnavailableError("injected")
        if tool in self.fail_tools:
            return ToolResponse(
                protocol_version=PROTOCOL_VERSION,
                request_id="test",
                timestamp="now",
                status="error",
                error={"code": "internal_error", "message": "injected broker error"},
            )
        if tool not in {
            "get_system_info",
            "get_cpu_info",
            "get_memory_info",
            "get_disk_info",
            "list_processes",
            "launch_application",
        }:
            return ToolResponse(
                protocol_version=PROTOCOL_VERSION,
                request_id="test",
                timestamp="now",
                status="error",
                error={"code": "unknown_tool", "message": f"unknown {tool}"},
            )
        result = self.responses.get(tool) or _default_result(tool, arguments)
        return ToolResponse(
            protocol_version=PROTOCOL_VERSION,
            request_id="test-req",
            timestamp="now",
            status="ok",
            result=result,
        )

    def shutdown(self) -> None:
        return None


def _default_result(tool: str, arguments: dict[str, Any]) -> dict[str, Any]:
    if tool == "get_memory_info":
        return {
            "total_bytes": 16_000_000_000,
            "used_bytes": 14_000_000_000,
            "used_percent": 87.5,
            "top_consumers": [
                {"pid": 4242, "name": "firefox", "bytes": 4_500_000_000},
            ],
        }
    if tool == "get_cpu_info":
        return {
            "model": "test-cpu",
            "core_count": 8,
            "usage_percent": 62.0,
            "per_core_usage_percent": [60.0] * 8,
        }
    if tool == "list_processes":
        return {
            "processes": [
                {
                    "pid": 4242,
                    "name": "firefox",
                    "cpu_percent": 40.0,
                    "memory_bytes": 4_500_000_000,
                },
                {
                    "pid": 100,
                    "name": "code",
                    "cpu_percent": 12.0,
                    "memory_bytes": 1_000_000_000,
                },
            ]
        }
    if tool == "get_disk_info":
        return {
            "volumes": [
                {
                    "mount_point": "/",
                    "total_bytes": 500_000_000_000,
                    "used_bytes": 200_000_000_000,
                    "used_percent": 40.0,
                }
            ]
        }
    if tool == "get_system_info":
        return {
            "os_name": "TuwaiqOS",
            "os_version": "v0.5",
            "kernel_version": "test",
            "hostname": "tuwaiq-dev",
            "uptime_seconds": 123,
        }
    if tool == "launch_application":
        return {"app_id": arguments.get("app_id"), "pid": 999, "launched": True}
    return {}
