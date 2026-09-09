from __future__ import annotations

import os

import pytest

from model_provider import LocalModelProvider


@pytest.mark.skipif(
    not os.getenv("TUWAIQ_RUN_QWEN_SMOKE"),
    reason="set TUWAIQ_RUN_QWEN_SMOKE=1 and TUWAIQ_AI_MODEL_PATH_DEFAULT to run the real local Qwen smoke test",
)
def test_qwen_local_smoke_hello_response(monkeypatch: pytest.MonkeyPatch) -> None:
    pytest.importorskip("llama_cpp")
    model_path = os.getenv("TUWAIQ_AI_MODEL_PATH_DEFAULT")
    if not model_path:
        pytest.skip("TUWAIQ_AI_MODEL_PATH_DEFAULT is required for the local Qwen smoke test")

    monkeypatch.setenv("TUWAIQ_AI_MODEL_PATH_DEFAULT", model_path)
    provider = LocalModelProvider(profile="default", require_model_file=True)
    try:
        action = provider.decide("Hello")
        assert action.kind == "respond"
        assert (action.text or "").strip()
    finally:
        provider.shutdown()
