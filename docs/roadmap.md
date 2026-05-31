# DOL Roadmap

This document outlines the future vision and planned features for the Docker Observability Language (DOL) beyond version 0.1.

## Phase 11: Export and Integrations
- **Prometheus Exporter:** A background mode that exposes DOL telemetry (and the output of defined `observe` queries) as Prometheus metrics.
- **Grafana Data Source:** A native Grafana plugin that allows writing DOL queries directly in Grafana dashboards.
- **Webhooks & Slack/Discord Integration:** Native integrations for the `alert` webhook action to send formatted messages directly to chat platforms.

## Phase 12: Distributed DOL (Swarm / Kubernetes)
- **Docker Swarm Support:** Extend queries to be Swarm-aware (`observe services`, `observe nodes`).
- **Kubernetes Adapter:** A backend adapter that translates DOL `observe containers` and `events` into Kubernetes API calls (Pods, Deployments, Events), making DOL a universal container query language.

## Phase 13: AI Insights Layer
- **LLM Integration:** Integrate local (Ollama) or remote (OpenAI/Anthropic) LLMs to provide natural language explanations for anomalies detected by the `analyze` engine.
- **Query Synthesis:** Allow users to write natural language ("find all containers that crashed yesterday") and compile it into DOL AST.
- **Root Cause Analysis (RCA):** Given an anomaly, automatically trace back through historical events and metrics to propose a probable root cause (e.g., "OOMKilled because memory limit was reached during a traffic spike").

## Phase 14: Advanced Query Features
- **Joins:** Allow joining different entity types (e.g., `observe containers join networks on network_id`).
- **Aggregations:** Support `group by` and aggregate functions like `sum`, `avg`, `max` in the pipeline (`observe containers | group by image | select image, avg(cpu)`).
- **Subqueries:** Allow using the result of one query as a filter in another.

## Phase 15: Performance and Scale
- **Parquet Storage Engine:** Replace or supplement SQLite with Apache Parquet for highly compressed, columnar storage of long-term metrics and events.
- **Distributed Querying:** Run a DOL agent on multiple host machines and aggregate results centrally.
