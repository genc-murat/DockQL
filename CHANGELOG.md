# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.0] - 2026-06-05

### Added

#### Configurable Docker API Timeouts

- **Timeout configuration system** — all Docker API call timeouts are now configurable via `dol config set` or YAML/TOML config file
- `DockerApiConfig` struct with `call_timeout`, `quick_timeout`, `max_retries`, `retry_base_ms` — read from `DolConfig` via `From<&DolConfig>`
- `docker_call()`, `docker_call_quick()`, `docker_call_with_retry()` — timeout-aware helper functions with exponential backoff retry
- `BollardDockerClient::connect_with_config()` — create client with configurable timeout settings
- `BollardMetricsCollector::with_config()` — create collector with configurable stats timeout
- `alerts::init_alert_timeouts()` — global `OnceLock`-based alert timeout initialization from config
- `DockerError::Timeout(u64)` — dedicated timeout error variant
- 6 new config keys: `api-timeout` (30s), `api-quick-timeout` (10s), `stats-timeout` (10s), `events-timeout` (30s), `webhook-timeout` (10s), `restart-timeout` (30s)
- 13 unit tests covering YAML/TOML round-trip, deserialization, `DockerApiConfig` conversion, and error handling

#### Parser Error Message Improvements

- **`expect_ident()` and `expect()` now show both expected and found tokens** — e.g., `expected \`then\`, found \`emai\`` instead of just `expected \`then\``
- **Keyword similarity suggestions** — `suggest_keyword()` with Levenshtein distance via `strsim` crate for pipeline nodes, analysis verbs, and alert actions (`fltr` → `fill`, `grp` → `group`, `prnt` → `print`, `fnd` → `find`)
- **`suggest_keyword(input, keywords)`** — reusable helper filtering by first character + `strsim::levenshtein()` minimum
- 9 new unit tests for keyword suggestions with close and distant misspellings

#### CLI Polish

- **Rich `--help` output** — `about` field rewritten with descriptive text; new `long_about` with DOL overview; `after_help` with 23 categorized example queries (basic, advanced, files/store, output/integration, interactive modes)
- **Improved subcommand descriptions** — `repl` (+tab completion, history, syntax-colored errors), `top` (+CPU, memory, network stats), `dashboard` (+event stream, resource gauges)
- **`dol config --help` enriched** — `ConfigAction` variants (`init`, `set`, `view`) now have doc comments with usage hints

#### API Documentation

- **Module-level `//!` docs with examples** — added to all 18 library modules (`alerts`, `ast`, `cli`, `config`, `dashboard`, `docker`, `eval`, `events`, `executor`, `export`, `lib`, `metrics`, `parser`, `planner`, `repl`, `semantic`, `sqlite_store`, `storage`)
- **`lib.rs` crate root doc** — module overview table with one-line descriptions for all 19 submodules
- All examples use `ignore` blocks (require Docker), format: description + `# Example` + code
- `cargo doc --no-deps`: 0 warnings

### Changed

#### Non-breaking

- `BollardDockerClient::connect()` now uses `DockerApiConfig::default()` for backward compatibility
- `BollardDockerClient::connect_with_host()` accepts `&DolConfig` for timeout configuration
- `str_similarity()` replaced with `strsim::levenshtein()` for accurate Levenshtein edit distance
- `suggest_similar_target()` refactored to use `suggest_keyword()` internally

#### Documentation

- **README.md restructured** — features list condensed from 30+ to 15 items; duplicate "Query Timeout" section removed; REPL/Dashboard sections shortened with links to docs/; new "Error Messages" section showcasing improved parser errors; CLI Flags table deduplicated
- **docs/spec.md** — new "Docker API Timeout Configuration" section with all 6 timeout keys, defaults, and example YAML
- **docs/architecture.md** — updated Docker Client (timeout/retry helpers), Alerting Engine (configurable timeouts), and Config Loader (12-key table) sections
- **docs/examples.md** — new section 32 "Configurable Timeouts" with `dol config set` examples and YAML config template
- **docs/examples.html** — regenerated from examples.md via pandoc, includes new section 32
- **docs/index.html** — feature grid updated with "Configurable Docker API timeouts (with retry)"
- Fixed `metrics_interval` (10→30) and `snapshot_interval` (60→300) defaults in architecture.md to match code constants

#### Code Quality

- 21 new tests (13 config + 8 parser)
- Test count: 413 total (+21), 0 failed, 1 ignored
- `cargo clippy` — zero warnings
- `cargo audit` — zero vulnerabilities
- **CI: example validation** — `scripts/validate_examples.sh` now runs in CI after tests, validating all 101 `.dol` example files with `dol --explain`
- **CI: rust-cache** — `Swatinem/rust-cache@v2` added for faster incremental builds

### Dependencies

- Added: `strsim = "0.11.1"` (Levenshtein distance for keyword suggestions)
- Updated: `bitflags` 2.11.1 → 2.12.1, `chrono` 0.4.44 → 0.4.45, `log` 0.4.30 → 0.4.32, `yoke` 0.8.2 → 0.8.3 (`cargo update`, 2026-06-05)
- Added: `DEPENDENCY_POLICY.md` — formal dependency upgrade policy (cadence, pinning, audit requirements, adding new dependencies)

## [Unreleased]

### Documentation

- **docs/spec.md — Language specification v0.1 → v0.2** — spec version bumped to reflect all features added since the original spec (window functions, `assert`/`debug`/`fill`/`let` pipeline nodes, `case/when`/`if` expressions, string operators, compose streaming, `from...to` historical ranges, type conversion functions, etc.)
- **docs/spec.md — Status: draft → stable** — language specification marked as stable; all described features are implemented across 7 tool releases
- **docs/spec.md — Section 13 renamed** — `MVP Acceptance Query Set` → `Example Query Set` (MVP terminology was outdated)
- **docs/spec.md — Section 13.1 numbering fixed** — `### 8.1 Aggregate Functions` → `### 13.1 Aggregate Functions` (was left over from when section belonged under Pipeline Syntax)
- **docs/spec.md — Historical ranges text corrected** — `reserved for v0.2, experimental in v0.1` → `fully supported via the telemetry store`

## [0.7.0] - 2026-06-06

### Added

#### Compose Streaming

- **Compose events (batch)** — `compose <project> events` collects Docker events filtered by compose project label with full pipeline support (`where`, `select`, `limit`, etc.). Note: this is a batch operation, not a live stream.
- **Compose logs streaming** — `compose <project> logs <service>` streams real-time container logs for a compose service with pipeline filtering (`where message contains "error"`, `select message, line`, `limit`) and `--timeout` support
- **Compose networks streaming** — `compose <project> networks` with a pipeline streams real-time Docker network events filtered by compose project label (`compose myapp networks | where action = connect | select time, actor_id`)
- **Container log streaming** — `logs container <name>` now supports streaming mode (follow) when a pipeline is present; pipeable with `where`, `select`, `limit`; `--timeout` auto-stops after a duration

#### REPL

- **Tab completion enhancements** — 23 new keywords added: compose sub-targets (`services`, `health`, `stats`, `ps`, `port`, `config`, `ls`), pipeline nodes (`having`, `offset`, `distinct`, `fill`, `let`, `debug`, `assert`), string operators (`starts_with`, `ends_with`, `between`), window functions (`row_number`, `rank`, `lag`, `lead`), and `end` for case/when expressions

#### Testing

- **`stream_compose_logs` unit tests** — 4 new tests with `MockDockerClient`: basic multi-container, `select` pipeline filtering, no-matching-containers early return, `limit` pipeline truncation
- **`stream_compose_networks` unit tests** — 4 new tests: basic filtering by compose project, non-network event filtering, `select` pipeline, `limit` pipeline

#### Documentation

- **README.md** — compose events, logs, and networks streaming examples in top examples, features, quick start, and bottom examples list
- **docs/spec.md** — sections 2.8 and 9.2 updated with compose networks streaming; execution mode updated for all streaming targets
- **docs/tutorial.md** — section 15 updated with compose events, compose logs streaming, compose networks streaming, and container log streaming examples
- **docs/examples.md** — compose streaming examples added to all relevant sections
- **docs/index.html, spec.html, tutorial.html, examples.html** — all HTML docs updated with compose streaming examples
- **docs/architecture.md** — executor section updated with compose streaming dispatch description

### Fixed

- **Compose events streaming documentation** — `compose myapp events` is a batch operation (collects events via `compose_events()` in executor), not true streaming. Removed from `--help` and `.help` streaming targets list (`events, logs, networks` → `logs, networks`). The batch query still works correctly.

## [0.5.0] - 2026-06-04

### Added

#### Docker SDK — Bollard Migration

- `BollardDockerClient` — native Rust Docker API integration via `bollard` crate, replacing `Command::new("docker")` CLI wrapper
- `DockerClient` trait with fully async methods: `list_containers`, `list_images`, `list_networks`, `list_volumes`, `inspect_container`, `container_logs`, `container_stats`, `events_stream`, `ping`
- `BollardMetricsCollector` — async metrics collection via bollard stats API (CPU, memory, network)
- `BollardEventSource` — async Docker event streaming via bollard events API
- `connect_with_host()` — remote Docker host support (tcp://, http://, unix://)
- `MockDockerClient` and `MockMetricsCollector` — test doubles for unit testing
- Async container inspection with health, restart count, and timestamp enrichment

#### Compose Queries

- `compose ls` — list all Docker Compose projects with container, network, and volume counts
- `compose <project> images` — list images used by a Compose project
- `compose <project> stats` — resource usage statistics for Compose project containers (CPU, memory, network, disk)
- `compose <project> ps` — enhanced container status for a Compose project with service names
- `compose <project> logs <service> [tail <n>]` — view logs for a specific service
- `compose <project> port <service> <port>` — show port mappings for a service
- `compose <project> config [services|networks|volumes]` — inspect Compose project configuration
- `compose <project> events` — query Compose project events (streaming)
- All new compose targets support full pipeline syntax (where, select, sort by, group by, limit, etc.)

#### Analysis Engine

- `analyze containers find leaks` — detect memory leaks from historical trend data
- `analyze containers find drift` — detect configuration drift between snapshots
- `analyze containers find density` — container distribution analysis by image, state, and compose project
- `analyze containers find dependencies` — dependency graph across compose projects, networks, and volumes
- `analyze containers explain` — diagnostic summary for all containers
- `explain container <name>` — per-container diagnostic with signals (CPU, memory, network, restart count)
- `AnalysisThresholds` — configurable thresholds for all detectors

#### CLI & Dashboard

- Event-driven TUI refresh — `dol top` and `dol dashboard` now listen to Docker events via bollard for instant UI updates
- `--diff` flag — compare current state with last store snapshot
- `--export-format` for InfluxDB, Grafana Loki, and Prometheus Pushgateway
#### Code Quality

- 44 new tests covering compose parser, executor, and semantic validation
- 10 new example `.dol` files (total: 101)
- Extensive clippy lint fixes: `#[must_use]`, `Self::` usage, `map_or`/`map_or_else` simplifications, `to_vec()` over `.iter().cloned().collect()`, let-else patterns, `expect()` over `unwrap()`
- `cargo clippy` — zero warnings

### Changed

#### Breaking

- `MetricsCollector::collect()` is now async (was sync)
- `DockerClient` methods are now async (were sync subprocess calls)
- `evaluate_alert_once()` is now async
- `execute_analyze()`, `execute_analyze_with_thresholds()` are now async

#### Non-breaking

- `ComposeTarget` enum extended with: `Projects`, `Images`, `Stats`, `Ps`, `Events`, `Port`, `Config`, `Logs`
- `ComposeQuery` struct now includes `service`, `port_number`, `tail`, and `config_target` fields
- Semantic analyzer validates fields for all new compose targets
- `execute_compose_projects` now applies pipeline nodes (where, select, sort, etc.)
- Alert webhook action uses dedicated tokio runtime (thread + block_on) for reliability
- Alert restart action migrated from `docker restart` subprocess to bollard `restart_container()` API
- Dashboard metrics collection now fully async (removed `Handle::current().block_on()`)
- `render_table_ratatui()` — box-drawing table with proper border rendering
- `render_table_colored()` — color-coded plain-text fallback for narrow terminals

#### Dependencies

- Added: `bollard = "0.21.0"`, `futures-util = "0.3.32"`, `tokio-stream = "0.1.18"`
- Removed implicit `docker` CLI dependency (no longer requires docker binary for data collection)

## [0.4.0] - 2026-06-02

### Added

- `--file` / `-f` CLI flag to read DOL queries directly from `.dol` files (e.g., `dol --file examples/ping.dol`)
- `--theme dark|light` CLI flag for table output colour theme (dark with DarkGray alternating rows, or light with blue headings, no row tint)
- `theme` config field — set default theme permanently via `dol config set theme light` (CLI `--theme` takes precedence over config)

### Changed

- `sort by size desc` now correctly sorts byte-size strings (MB, GB, etc.) using log-scale numeric comparison
- Table renderer improvements: hidden empty columns, capped column widths (30 chars) with truncation, log10-scaled visual size bars, `─` separator
- `--theme` CLI flag changed from required default to `Option<Theme>` to allow config fallback

### Documentation

- Updated README.md with `--theme` flag, config file `theme` field, and light/dark theme descriptions
- Updated docs/spec.md CLI Reference and config set key list with `--theme` / `theme`
- Updated docs/tutorial.md quick tips with `--theme light` usage note
- Updated docs/architecture.md with theme resolution flow (CLI > config > default)
- Updated docs/index.html landing page with theme example commands

## [0.3.0] - 2026-06-01

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

[0.3.0]: https://github.com/genc-murat/DockQL/compare/v0.2.0...v0.3.0

[0.4.0]: https://github.com/genc-murat/DockQL/compare/v0.3.0...v0.4.0

[0.5.0]: https://github.com/genc-murat/DockQL/compare/v0.4.0...v0.5.0

[0.7.0]: https://github.com/genc-murat/DockQL/compare/v0.6.0...v0.7.0
[Unreleased]: https://github.com/genc-murat/DockQL/compare/v0.7.0...HEAD
[0.6.0]: https://github.com/genc-murat/DockQL/compare/v0.5.0...v0.6.0
