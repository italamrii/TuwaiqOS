# Future Integration Specification

This document defines conceptual interfaces for future TuwaiqOS integration.
No kernel integration is implemented in this prototype.

## 1) System Telemetry Provider (future)

Purpose:
- Provide read-only system telemetry snapshots to the AI System Intelligence service.

Expected metrics:
- CPU utilization
- RAM utilization and available RAM
- Process count
- Disk read/write activity
- Network inbound/outbound activity
- Uptime
- Error/event count
- Service state

Input schema reference:
- ../schemas/telemetry_input.schema.json

Integration status:
- Future integration requirement

## 2) AI Inference Service

Purpose:
- Receive telemetry and return anomaly detection result.

Output fields:
- anomaly_detected
- anomaly_score
- severity
- model_version
- indicators

Output schema reference:

- ../schemas/inference_output.schema.json

Integration status:
- Prototype implemented in isolated Python component
- Kernel integration is future work

## 3) Future AI Assistant

Future questions that the assistant should map into structured requests:
- Why is the system slow?
- Is the current system state abnormal?
- What resources are under pressure?
- Which process appears associated with anomaly patterns?
- Has system behavior changed significantly?

Important boundary:
- The assistant must not access the kernel directly.
- All requests must pass through policy validation and approved system APIs.

## Deterministic Request/Response Examples

Request:
{
  "timestamp": "2026-01-01T00:00:15Z",
  "cpu_utilization_pct": 95,
  "ram_utilization_pct": 96,
  "available_ram_mb": 300,
  "process_count": 220,
  "disk_read_kbps": 10000,
  "disk_write_kbps": 9000,
  "network_in_kbps": 11000,
  "network_out_kbps": 10000,
  "uptime_seconds": 3615,
  "error_event_count": 10,
  "service_state": "critical"
}

Response shape:
{
  "anomaly_detected": true,
  "anomaly_score": 0.37,
  "severity": "high",
  "model_version": "v0.1.0",
  "indicators": ["cpu_pressure", "memory_pressure", "disk_io_pressure", "network_pressure"]
}
