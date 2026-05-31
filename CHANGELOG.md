# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Initial release of DOL (Docker Observability Language)
- Live observation of containers, images, networks, and volumes
- Real-time Docker event streaming with pipeline filters
- Historical inspection and observation with SQLite-backed telemetry store
- Alerting system with duration guards, webhooks, and restart actions
- Static semantic analysis and type checking for queries
- Terminal dashboard with live container monitoring (`dol top`, `dol dashboard`)
- Interactive REPL with tab completion
- Multiple output formats: table, CSV, JSON, JSONL
- External integrations: InfluxDB, Grafana Loki, Prometheus Pushgateway
- Aggregation functions, arithmetic expressions, string functions
- Conditional pipeline branching (`if/then/else`, `case/when`)
- Multi-field sorting, `having`, `distinct`, `offset` pipeline nodes
- Config file support (YAML/TOML)

[Unreleased]: https://github.com/genc-murat/DockQL/compare/v0.1.0...HEAD
