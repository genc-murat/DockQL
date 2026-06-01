# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `let` pipeline node for declaring constants and parameters (`let $threshold = 80`)
- `fill` pipeline node to replace null/missing values with a default (`fill memory with 0`)
- `starts_with` and `ends_with` comparison operators (`where name starts_with "api-"`)
- String functions: `starts_with`, `ends_with`, `replace`, `reverse`, `repeat`, `position`, `split_part`
- Date/time functions: `now()`, `date_format()`, `date_diff()`, `extract()`
- `$var` field reference syntax for explicit field access (`where $state = running`)
- Expressions in `set case/when` and `set if/then/else` branches (e.g., `concat()` in `then`)
- 29 new example `.dol` files showcasing all new features
- `scripts/validate_examples.sh` — standalone validation script for example files
- Advanced pipeline examples combining all features (monitoring, alerting, time analysis, etc.)

### Changed

- Tokenizer: `-` is now properly parsed as minus operator in arithmetic expressions
- `case/when` and `if/then/else` in `set` now accept full expressions (function calls, arithmetic) in result branches

### Documentation

- Updated README.md with new features, pipeline nodes table, and 83 example queries
- Updated docs/spec.md with fill, let, string/date functions, $var syntax, and starts_with/ends_with operators
- Updated docs/tutorial.md with new sections for fill, let, date/time functions, and $var
- Updated docs/examples.md with examples for all new features
- Updated docs/index.html with updated features list and example count

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
