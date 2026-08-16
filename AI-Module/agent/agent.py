"""Bounded grounded agent orchestration.

Security invariants:
- Never calls subprocess/os.system/etc. directly.
- Only reaches the OS via BrokerClient after schema + policy checks.
- User/model text is untrusted; only broker results are trusted evidence.
- Confirmation for sensitive intent is tracked outside model text.
"""

from __future__ import annotations

import logging
import re
from typing import Any

from broker_client import BrokerClient, BrokerUnavailableError
from config import AgentConfig
from memory import SessionMemory
from model_provider import AgentAction, ModelProvider, ProviderFailure
from policy import RiskClass, decide_policy, looks_like_shell_payload
from protocol import KNOWN_TOOLS
from tool_catalog import FORBIDDEN_TOOLS, RESERVED_SENSITIVE_TOOLS, validate_arguments

logger = logging.getLogger("tuwaiq_agent.agent")

_CONFIRM_YES = re.compile(
    r"^\s*(yes|y|confirm|موافق|نعم|أجل|اجل|أكد|اكد)\s*[.!؟]?\s*$",
    re.IGNORECASE,
)
_CONFIRM_NO = re.compile(
    r"^\s*(no|n|cancel|رفض|لا|ألغ|الغ)\s*[.!؟]?\s*$",
    re.IGNORECASE,
)


class Agent:
    def __init__(
        self,
        model: ModelProvider,
        broker: BrokerClient,
        config: AgentConfig | None = None,
        memory: SessionMemory | None = None,
    ):
        self._model = model
        self._broker = broker
        self._config = config or AgentConfig.from_environ()
        self._memory = memory or SessionMemory(
            max_turns=self._config.max_session_turns,
            max_evidence=self._config.max_evidence_records,
            evidence_ttl_seconds=self._config.evidence_ttl_seconds,
        )
        self._audit_events: list[dict[str, Any]] = []

    @property
    def memory(self) -> SessionMemory:
        return self._memory

    @property
    def audit_events(self) -> list[dict[str, Any]]:
        return list(self._audit_events)

    def handle(self, user_message: str) -> str:
        self._memory.add_user(user_message)

        # External confirmation turn — never inferred from model prose alone.
        if self._memory.pending_confirmation is not None:
            reply = self._handle_confirmation_turn(user_message)
            self._memory.add_assistant(reply)
            return reply

        try:
            reply = self._run_bounded_loop(user_message)
        except Exception:
            logger.exception("unexpected agent failure")
            reply = "Sorry, I ran into a problem. Could you try again?"
        self._memory.add_assistant(reply)
        return reply

    def _handle_confirmation_turn(self, user_message: str) -> str:
        pending = self._memory.pending_confirmation or {}
        if _CONFIRM_NO.match(user_message):
            self._audit(
                event="confirmation_denied",
                tool=pending.get("tool"),
                risk=pending.get("risk"),
            )
            self._memory.clear_pending_confirmation()
            return "تم الإلغاء. لن أنفّذ هذا الإجراء."

        if not _CONFIRM_YES.match(user_message):
            return (
                "بانتظار تأكيد صريح (نعم/لا). "
                f"الإجراء المطلوب: {pending.get('summary', 'إجراء حسّاس')}."
            )

        self._audit(
            event="confirmation_accepted",
            tool=pending.get("tool"),
            risk=pending.get("risk"),
        )
        self._memory.clear_pending_confirmation()
        # V1 broker has no process termination capability.
        self._audit(
            event="deferred_capability",
            tool=pending.get("tool"),
            reason="broker_v1_no_terminate",
        )
        return (
            "تم تسجيل تأكيدك، لكن إيقاف العمليات غير متاح في الوسيط الحالي (V1). "
            "لن أنفّذ إنهاء أي عملية، ولا يمكن تجاوز هذه السياسة عبر نص النموذج."
        )

    def _run_bounded_loop(self, user_message: str) -> str:
        history = [
            {"role": t.role, "content": t.content}
            for t in self._memory.turns
            if t.role in {"user", "assistant"}
        ]

        for iteration in range(self._config.max_iterations):
            evidence = self._memory.evidence_summary()
            try:
                decision = self._model.decide(
                    user_message,
                    evidence_summary=evidence,
                    history=history,
                )
            except Exception:
                logger.exception("model provider raised while deciding")
                self._audit(event="provider_failure", code="exception")
                return "Sorry, I ran into a problem understanding that. Could you try again?"

            if isinstance(decision, ProviderFailure):
                self._audit(
                    event="provider_failure",
                    code=decision.code,
                    message=decision.message,
                )
                if evidence:
                    return self._safe_finalize(user_message, evidence)
                return (
                    "Local model is temporarily unavailable "
                    f"({decision.code}). Please try again shortly."
                )

            if decision.kind == "respond":
                if decision.text:
                    return decision.text
                return self._safe_finalize(user_message, evidence)

            tool_reply = self._handle_tool_call(user_message, decision, iteration)
            if tool_reply is not None:
                return tool_reply

        # Budget exhausted — return grounded partial results if any.
        self._audit(event="budget_exhausted", max_iterations=self._config.max_iterations)
        evidence = self._memory.evidence_summary()
        if evidence:
            partial = self._safe_finalize(user_message, evidence)
            return partial + " (reached the tool-iteration budget; results above are partial.)"
        return "I could not finish within the allowed tool budget. Please try a narrower question."

    def _handle_tool_call(
        self,
        user_message: str,
        action: AgentAction,
        iteration: int,
    ) -> str | None:
        """Execute one tool call. Return a user reply to stop the loop, or None to continue."""
        tool = action.tool or ""
        arguments = dict(action.arguments or {})

        self._audit(
            event="tool_requested",
            tool=tool,
            arguments=_redact(arguments),
            iteration=iteration,
        )

        if tool in FORBIDDEN_TOOLS or looks_like_shell_payload(arguments):
            self._audit(event="policy_denied", tool=tool, risk=RiskClass.FORBIDDEN.value)
            return "I cannot run shell commands or arbitrary system actions."

        schema_error = validate_arguments(tool, arguments)
        if schema_error and tool in KNOWN_TOOLS:
            self._audit(event="schema_rejected", tool=tool, reason=schema_error)
            return f"I couldn't form a valid tool request ({schema_error})."

        # Sensitive reserved tools: confirmation / deferred path — never broker execution.
        if tool in RESERVED_SENSITIVE_TOOLS:
            policy = decide_policy(tool, arguments, confirmed=False)
            self._audit(
                event="policy_decision",
                tool=tool,
                risk=policy.risk.value,
                allowed=policy.allowed,
                requires_confirmation=policy.requires_confirmation,
                reason=policy.reason,
            )
            focus = self._memory.last_focus_process or {}
            target = arguments.get("name") or focus.get("name") or arguments.get("pid") or "unknown"
            summary = (
                f"إنهاء العملية المستهدفة ({target}). "
                "هذا إجراء حسّاس وغير منفَّذ في الوسيط الحالي."
            )
            self._memory.set_pending_confirmation(
                {
                    "tool": tool,
                    "arguments": arguments,
                    "risk": policy.risk.value,
                    "summary": summary,
                    "target": target,
                }
            )
            self._audit(event="confirmation_required", tool=tool, target=target)
            return (
                f"{summary}\n"
                "للتأكيد اكتب «نعم». للإلغاء اكتب «لا». "
                "لن أنفّذ شيئاً قبل تأكيد منفصل خارج نص النموذج."
            )

        if tool not in KNOWN_TOOLS:
            self._audit(event="unknown_tool", tool=tool)
            return "I don't have a way to do that yet."

        policy = decide_policy(tool, arguments, confirmed=False)
        self._audit(
            event="policy_decision",
            tool=tool,
            risk=policy.risk.value,
            allowed=policy.allowed,
            requires_confirmation=policy.requires_confirmation,
            reason=policy.reason,
        )
        if not policy.allowed:
            return f"That action was denied by policy ({policy.reason})."

        try:
            response = self._broker.call(tool, arguments)
        except BrokerUnavailableError as exc:
            logger.error("broker unavailable: %s", exc)
            self._audit(event="broker_unavailable", tool=tool)
            return "System tools are temporarily unavailable. Please try again shortly."

        if response.ok:
            result = response.result or {}
            self._memory.add_evidence(
                tool=tool,
                result=result,
                request_id=response.request_id,
                user_message=user_message,
            )
            self._audit(
                event="tool_ok",
                tool=tool,
                request_id=response.request_id,
            )
            # Continue the bounded loop so the model can gather more evidence
            # or produce a final grounded answer.
            return None

        self._audit(
            event="tool_error",
            tool=tool,
            request_id=response.request_id,
            error_code=response.error_code,
        )
        try:
            return self._model.explain_error(
                user_message,
                tool,
                response.error_code or "internal_error",
                response.error_message or "",
            )
        except Exception:
            logger.exception("model provider raised while explaining an error")
            return "I couldn't complete that request."

    def _safe_finalize(self, user_message: str, evidence: list[dict[str, Any]]) -> str:
        try:
            return self._model.finalize(user_message, evidence)
        except Exception:
            logger.exception("finalize failed")
            if not evidence:
                return "I could not produce a grounded answer without evidence."
            # Extremely defensive fallback: dump tool names only, not raw secrets.
            tools = ", ".join(e["tool"] for e in evidence)
            return f"Collected trusted evidence from: {tools}."

    def _audit(self, **fields: Any) -> None:
        event = {k: v for k, v in fields.items() if v is not None}
        self._audit_events.append(event)
        logger.info("audit %s", event)


def _redact(arguments: dict[str, Any]) -> dict[str, Any]:
    """Audit helper: never log values that look like secrets."""
    redacted: dict[str, Any] = {}
    for key, value in arguments.items():
        key_l = str(key).lower()
        if any(s in key_l for s in ("password", "token", "secret", "api_key", "authorization")):
            redacted[key] = "[redacted]"
        else:
            redacted[key] = value
    return redacted
