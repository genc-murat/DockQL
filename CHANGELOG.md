# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-06-01

### Added

- Compose query support with targets: containers, services, networks, volumes, health
- Join support for observe queries across collection targets
- `logs` command to view container logs with tail and filter options
- `ping` command to check Docker daemon connectivity
- Container analysis features: dependencies, density, leaks, drift
- `json-compact` output format
- `--timeout` flag for query execution
- `.host` REPL command to show or set Docker host
- GitHub Pages documentation site with modern DOL branding
- Homebrew installation support (`_brew/dol.rb`)
- Shell installer script (`install.sh`)

### Changed

- Enhanced error reporting with visual cues in parser and REPL
- Enhanced event-driven refresh model for dashboard and top commands
- Enhanced `group_rows` function with aggregate support
- Redesigned documentation with modern DOL branding theme
- DOCKER_HOST is now read from CLI args and config before command execution

### Fixed

- Various documentation formatting and CI workflow fixes

## [0.1.1] - 2026-05-31

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

[Unreleased]: https://github.com/genc-murat/DockQL/compare/v0.1.1...v0.2.0

[Unreleased]: https://github.com/genc-murat/DockQL/compare/v0.2.0...HEAD
