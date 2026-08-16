# Integration Guide (Future TuwaiqOS Integration)

This document specifies what maintainers would need to integrate this prototype later.
No integration is implemented in this prototype.

## 1) Integration Objectives

- Provide a read-only telemetry feed to the intelligence service.
- Run model inference on telemetry snapshots.
- Return anomaly signals to approved consumers (shell/app/assistant).

## 2) Required Integration Interfaces

A) System Telemetry Provider
- Must emit schema-compatible telemetry objects.
- Must expose CPU, memory, process, storage, network, uptime, and event metrics.
- Must label unavailable metrics as future integration requirement until implemented.

B) AI Inference Service Adapter
- Loads exported model.
- Validates incoming telemetry.
- Returns schema-defined anomaly response.

C) Assistant Query Adapter
- Converts natural-language requests into structured telemetry/inference queries.
- Must pass policy validation before API access.

## 3) Security Requirements

- No direct LLM to kernel access.
- No arbitrary shell command execution from AI assistant.
- Read-only default behavior.
- No automatic process termination.
- No automatic system modification.
- No telemetry exfiltration unless explicitly configured.

## 4) Suggested Integration Stages

Stage 1:
- Introduce telemetry snapshot provider in a stable API layer.

Stage 2:
- Add userland inference adapter that consumes telemetry snapshots.

Stage 3:
- Add assistant context-manager and policy-layer wiring.

Stage 4:
- Add observability, guardrails, and controlled rollout.

## 5) Validation Checklist for Maintainers

- Telemetry matches input schema.
- Inference output matches output schema.
- Deterministic scenario tests pass.
- Model version metadata is tracked.
- Detection output is not misrepresented as diagnosis.
