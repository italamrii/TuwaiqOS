"""Minimal CLI for the Tuwaiq AI agent prototype.

Per the task requirements: "Start with CLI or minimal UI. Do NOT spend time
building the final graphical assistant yet." This is intentionally a plain
REPL -- the point of Phase 1 is proving the agent/broker/protocol
architecture works end to end, not the UI.

Phase 4 update: a single ConversationContext is maintained for the whole
session so follow-up questions ("What's using the most?", "Open it.") work
correctly across turns.

Run: python cli.py
"""

from __future__ import annotations

import logging
import sys

from agent import Agent
from broker_client import BrokerClient
from conversation_context import ConversationContext
from model_provider import LocalModelProvider

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(name)s %(levelname)s %(message)s")


def main() -> None:
    print("Tuwaiq AI (prototype) — type 'exit' to quit.\n")
    broker = BrokerClient()
    model = LocalModelProvider(profile="default")
    agent = Agent(model=model, broker=broker)
    context = ConversationContext()

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

            reply = agent.handle_with_context(user_input, context)
            print(f"Tuwaiq: {reply}\n")
    finally:
        model.shutdown()
        broker.shutdown()
        print("Goodbye.")


if __name__ == "__main__":
    sys.exit(main() or 0)
