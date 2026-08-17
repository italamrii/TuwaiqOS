"""Minimal CLI for the grounded Tuwaiq AI agent V1.

Run from AI-Module/agent:
  TUWAIQ_AI_PROVIDER=rule python cli.py
  TUWAIQ_AI_PROVIDER=local python cli.py
"""

from __future__ import annotations

import logging
import sys

from agent import Agent
from broker_client import BrokerClient
from config import AgentConfig
from model_provider import build_provider

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s %(name)s %(levelname)s %(message)s",
)


def main() -> None:
    config = AgentConfig.from_environ()
    print(
        "Tuwaiq AI grounded agent V1 — type 'exit' to quit.\n"
        f"provider={config.provider} model={config.model_id} "
        f"max_iterations={config.max_iterations}\n"
    )
    broker = BrokerClient(config.resolve_broker_path())
    model = build_provider(config)
    agent = Agent(model=model, broker=broker, config=config)

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

            reply = agent.handle(user_input)
            print(f"Tuwaiq: {reply}\n")
    finally:
        broker.shutdown()
        print("Goodbye.")


if __name__ == "__main__":
    sys.exit(main() or 0)
