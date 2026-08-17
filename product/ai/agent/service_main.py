"""systemd entrypoint for tuwaiq-ai.service (local socket API)."""

from __future__ import annotations

import logging
import os
import signal
import sys

from config import AgentConfig
from runtime_status import DEFAULT_API_SOCKET
from socket_api import AgentSocketServer

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s %(name)s %(levelname)s %(message)s",
)
logger = logging.getLogger("tuwaiq_agent.service")


def main() -> int:
    config = AgentConfig.from_environ()
    socket_path = os.environ.get("TUWAIQ_AI_SOCKET", DEFAULT_API_SOCKET)
    server = AgentSocketServer(socket_path=socket_path, config=config)

    def _stop(signum: int, _frame: object) -> None:
        logger.info("received signal %s; shutting down", signum)
        server.stop()

    signal.signal(signal.SIGTERM, _stop)
    signal.signal(signal.SIGINT, _stop)
    logger.info(
        "starting tuwaiq-ai service provider=%s model=%s socket=%s",
        config.provider,
        config.model_id,
        socket_path,
    )
    try:
        server.serve_forever()
    except Exception:
        logger.exception("tuwaiq-ai service crashed")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
