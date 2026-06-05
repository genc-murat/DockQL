//! Deterministic analysis and insight engine.
//!
//! Provides anomaly detection, pattern recognition, and diagnostic summaries
//! for Docker containers based on metrics, events, and state snapshots.
//! All analyses are fully deterministic — no AI/LLM calls are made.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;
use serde_json::{Number, Value as JsonValue};

use crate::{
    ast::{AnalysisTarget, AnalysisVerb, AnalyzeQuery, CollectionTarget, SingularTargetKind},
    docker::{Container, DockerClient, DockerError, DockerEvent, MetricSample},
    executor::{ExecutionResult, Row},
    metrics::{MetricsCollector, MetricsError},
    storage::{TelemetryError, TelemetryStore},
    ONE_GB,
};

// ── Public error type ──────────────────────────────────────────────────────

/// Errors that can occur during analysis.
#[derive(Debug, thiserror::Error)]
pub enum AnalyzeError {
    #[error("{0}")]
    Docker(#[from] DockerError),
    #[error("{0}")]
    Metrics(#[from] MetricsError),
    #[error("{0}")]
    Telemetry(#[from] TelemetryError),
    #[error("analysis not supported for this target/verb combination")]
    Unsupported,
}

// ── Anomaly / Insight types ────────────────────────────────────────────────

/// Severity level for a detected anomaly.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Info => write!(f, "info"),
            Self::Warning => write!(f, "warning"),
            Self::Critical => write!(f, "critical"),
        }
    }
}

/// A single detected anomaly or insight.
#[derive(Debug, Clone, Serialize)]
pub struct Anomaly {
    pub severity: Severity,
    pub kind: String,
    pub container: String,
    pub message: String,
    /// Supporting evidence: metric values, event timestamps, etc.
    pub evidence: Vec<String>,
}

impl Anomaly {
    fn to_row(&self) -> Row {
        Row {
            fields: BTreeMap::from([
                (
                    "severity".to_owned(),
                    JsonValue::String(self.severity.to_string()),
                ),
                ("kind".to_owned(), JsonValue::String(self.kind.clone())),
                (
                    "container".to_owned(),
                    JsonValue::String(self.container.clone()),
                ),
                (
                    "message".to_owned(),
                    JsonValue::String(self.message.clone()),
                ),
                (
                    "evidence".to_owned(),
                    JsonValue::Array(
                        self.evidence
                            .iter()
                            .map(|e| JsonValue::String(e.clone()))
                            .collect(),
                    ),
                ),
            ]),
        }
    }
}

/// Summary of a single container's health signals.
#[derive(Debug, Clone, Serialize)]
pub struct ContainerExplanation {
    pub container: String,
    pub state: String,
    pub anomalies: Vec<Anomaly>,
    pub signals: Vec<Signal>,
}

/// A diagnostic signal extracted from telemetry data.
#[derive(Debug, Clone, Serialize)]
pub struct Signal {
    pub name: String,
    pub value: String,
    pub status: SignalStatus,
}

/// Status classification for a signal.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub enum SignalStatus {
    Normal,
    Elevated,
    Critical,
}

impl std::fmt::Display for SignalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Normal => write!(f, "normal"),
            Self::Elevated => write!(f, "elevated"),
            Self::Critical => write!(f, "critical"),
        }
    }
}

// ── Thresholds ─────────────────────────────────────────────────────────────

/// Configurable thresholds for anomaly detection.
#[derive(Debug, Clone)]
pub struct AnalysisThresholds {
    /// CPU percentage above which a container is considered high CPU.
    pub high_cpu_percent: f64,
    /// CPU percentage above which a container is critical.
    pub critical_cpu_percent: f64,
    /// Memory usage ratio (usage/limit) above which a container is under pressure.
    pub memory_pressure_ratio: f64,
    /// Memory usage ratio above which a container is critical.
    pub critical_memory_ratio: f64,
    /// Number of restarts indicating a restart loop.
    pub restart_loop_count: u64,
    /// Number of "die" events in recent history that indicate deployment failure.
    pub deployment_error_threshold: u64,
    /// Memory usage increase percentage indicating a resource leak (e.g., 20 = 20% increase).
    pub resource_leak_memory_increase_pct: f64,
    /// Minimum number of metric samples needed to detect a trend.
    pub resource_leak_min_samples: u64,
}

impl Default for AnalysisThresholds {
    fn default() -> Self {
        Self {
            high_cpu_percent: 80.0,
            critical_cpu_percent: 95.0,
            memory_pressure_ratio: 0.85,
            critical_memory_ratio: 0.95,
            restart_loop_count: 3,
            deployment_error_threshold: 3,
            resource_leak_memory_increase_pct: 20.0,
            resource_leak_min_samples: 3,
        }
    }
}

// ── Main analysis dispatcher ───────────────────────────────────────────────

/// Execute an analysis query using live Docker data + metrics.
pub async fn execute_analyze<C, M>(
    query: &AnalyzeQuery,
    docker: &C,
    metrics: &M,
) -> Result<ExecutionResult, AnalyzeError>
where
    C: DockerClient + ?Sized,
    M: MetricsCollector + ?Sized,
{
    execute_analyze_with_thresholds(query, docker, metrics, &AnalysisThresholds::default()).await
}

/// Execute an analysis query with custom thresholds.
pub async fn execute_analyze_with_thresholds<C, M>(
    query: &AnalyzeQuery,
    docker: &C,
    metrics: &M,
    thresholds: &AnalysisThresholds,
) -> Result<ExecutionResult, AnalyzeError>
where
    C: DockerClient + ?Sized,
    M: MetricsCollector + ?Sized,
{
    match (&query.target, &query.verb, query.subject.as_deref()) {
        // analyze containers find anomalies (default)
        (AnalysisTarget::Collection(CollectionTarget::Containers), AnalysisVerb::Find,
None | Some("anomalies")) => find_container_anomalies(docker, metrics, thresholds).await,
        // analyze containers find dependencies
        (AnalysisTarget::Collection(CollectionTarget::Containers), AnalysisVerb::Find,
            Some("dependencies")) => analyze_dependencies(docker).await,
        // analyze containers find density
        (AnalysisTarget::Collection(CollectionTarget::Containers), AnalysisVerb::Find,
            Some("density")) => analyze_density(docker).await,
        // analyze containers find leaks or drift — requires store, return error

        // analyze containers correlate
        (AnalysisTarget::Collection(CollectionTarget::Containers), AnalysisVerb::Correlate, _) => {
            correlate_containers(docker, metrics, thresholds).await
        }
        // analyze container <name> explain
        (AnalysisTarget::Singular(target), AnalysisVerb::Explain, _)
            if target.kind == SingularTargetKind::Container =>
        {
            explain_container(&target.value, docker, metrics, thresholds).await
        }
        // analyze containers explain (explain all)
        (AnalysisTarget::Collection(CollectionTarget::Containers), AnalysisVerb::Explain, _) => {
            explain_all_containers(docker, metrics, thresholds).await
        }
        _ => Err(AnalyzeError::Unsupported),
    }
}

/// Execute an analysis query using historical data from a telemetry store.
pub fn execute_analyze_with_store<S>(
    query: &AnalyzeQuery,
    store: &S,
) -> Result<ExecutionResult, AnalyzeError>
where
    S: TelemetryStore + ?Sized,
{
    let thresholds = AnalysisThresholds::default();

    match (&query.target, &query.verb, query.subject.as_deref()) {
        // analyze containers find anomalies (default, from store)
        (AnalysisTarget::Collection(CollectionTarget::Containers), AnalysisVerb::Find,
None | Some("anomalies")) => find_anomalies_from_store(store, &thresholds),
        // analyze containers find leaks (from store)
        (AnalysisTarget::Collection(CollectionTarget::Containers), AnalysisVerb::Find,
            Some("leaks")) => {
            let anomalies = detect_resource_leaks(store, &thresholds);
            if anomalies.is_empty() {
                Ok(ExecutionResult {
                    rows: vec![Row {
                        fields: BTreeMap::from([
                            ("severity".to_owned(), JsonValue::String("info".to_owned())),
                            (
                                "kind".to_owned(),
                                JsonValue::String("no_leaks_detected".to_owned()),
                            ),
                            ("container".to_owned(), JsonValue::String("*".to_owned())),
                            (
                                "message".to_owned(),
                                JsonValue::String(
                                    "No resource leaks detected in stored data".to_owned(),
                                ),
                            ),
                            ("evidence".to_owned(), JsonValue::Array(Vec::new())),
                        ]),
                    }],
                })
            } else {
                Ok(ExecutionResult {
                    rows: anomalies.iter().map(Anomaly::to_row).collect(),
                })
            }
        }
        // analyze containers find drift (from store)
        (
            AnalysisTarget::Collection(CollectionTarget::Containers),
            AnalysisVerb::Find,
            Some("drift"),
        ) => {
            let anomalies = detect_config_drift(store)?;
            if anomalies.is_empty() {
                Ok(ExecutionResult {
                    rows: vec![Row {
                        fields: BTreeMap::from([
                            ("severity".to_owned(), JsonValue::String("info".to_owned())),
                            ("kind".to_owned(), JsonValue::String("no_drift".to_owned())),
                            ("container".to_owned(), JsonValue::String("*".to_owned())),
                            (
                                "message".to_owned(),
                                JsonValue::String("No configuration drift detected".to_owned()),
                            ),
                            ("evidence".to_owned(), JsonValue::Array(Vec::new())),
                        ]),
                    }],
                })
            } else {
                Ok(ExecutionResult {
                    rows: anomalies.iter().map(Anomaly::to_row).collect(),
                })
            }
        }
        _ => Err(AnalyzeError::Unsupported),
    }
}

// ── Detectors ──────────────────────────────────────────────────────────────

/// Detect restart loops: containers with `restart_count` >= threshold.
#[must_use]
pub fn detect_restart_loops(containers: &[Container], threshold: u64) -> Vec<Anomaly> {
    containers
        .iter()
        .filter_map(|c| {
            let count = c.restart_count.unwrap_or(0);
            if count >= threshold {
                let severity = if count >= threshold * 2 {
                    Severity::Critical
                } else {
                    Severity::Warning
                };
                Some(Anomaly {
                    severity,
                    kind: "restart_loop".to_owned(),
                    container: c.name.clone(),
                    message: format!(
                        "Container '{}' has restarted {} times (threshold: {})",
                        c.name, count, threshold
                    ),
                    evidence: vec![
                        format!("restart_count={}", count),
                        format!("state={}", c.state),
                        format!("status={}", c.status),
                    ],
                })
            } else {
                None
            }
        })
        .collect()
}

/// Detect high CPU usage anomalies.
#[must_use]
pub fn detect_high_cpu(
    samples: &[MetricSample],
    high_threshold: f64,
    critical_threshold: f64,
) -> Vec<Anomaly> {
    samples
        .iter()
        .filter_map(|s| {
            let cpu = s.cpu_percent?;
            if cpu >= critical_threshold {
                Some(Anomaly {
                    severity: Severity::Critical,
                    kind: "high_cpu".to_owned(),
                    container: s.container_name.clone(),
                    message: format!(
                        "Container '{}' CPU at {:.1}% (critical threshold: {:.0}%)",
                        s.container_name, cpu, critical_threshold
                    ),
                    evidence: vec![
                        format!("cpu={:.1}%", cpu),
                        format!("timestamp={}", s.timestamp),
                    ],
                })
            } else if cpu >= high_threshold {
                Some(Anomaly {
                    severity: Severity::Warning,
                    kind: "high_cpu".to_owned(),
                    container: s.container_name.clone(),
                    message: format!(
                        "Container '{}' CPU at {:.1}% (warning threshold: {:.0}%)",
                        s.container_name, cpu, high_threshold
                    ),
                    evidence: vec![
                        format!("cpu={:.1}%", cpu),
                        format!("timestamp={}", s.timestamp),
                    ],
                })
            } else {
                None
            }
        })
        .collect()
}

/// Detect memory pressure: containers with memory usage/limit ratio above threshold.
#[must_use]
pub fn detect_memory_pressure(
    samples: &[MetricSample],
    pressure_ratio: f64,
    critical_ratio: f64,
) -> Vec<Anomaly> {
    samples
        .iter()
        .filter_map(|s| {
            let usage = s.memory_usage_bytes? as f64;
            let limit = s.memory_limit_bytes? as f64;
            if limit <= 0.0 {
                return None;
            }
            let ratio = usage / limit;
            if ratio >= critical_ratio {
                Some(Anomaly {
                    severity: Severity::Critical,
                    kind: "memory_pressure".to_owned(),
                    container: s.container_name.clone(),
                    message: format!(
                        "Container '{}' memory at {:.1}% of limit (critical: {:.0}%)",
                        s.container_name,
                        ratio * 100.0,
                        critical_ratio * 100.0
                    ),
                    evidence: vec![
                        format!("memory_usage={}", s.memory_usage_bytes.unwrap_or(0)),
                        format!("memory_limit={}", s.memory_limit_bytes.unwrap_or(0)),
                        format!("ratio={:.2}%", ratio * 100.0),
                    ],
                })
            } else if ratio >= pressure_ratio {
                Some(Anomaly {
                    severity: Severity::Warning,
                    kind: "memory_pressure".to_owned(),
                    container: s.container_name.clone(),
                    message: format!(
                        "Container '{}' memory at {:.1}% of limit (warning: {:.0}%)",
                        s.container_name,
                        ratio * 100.0,
                        pressure_ratio * 100.0
                    ),
                    evidence: vec![
                        format!("memory_usage={}", s.memory_usage_bytes.unwrap_or(0)),
                        format!("memory_limit={}", s.memory_limit_bytes.unwrap_or(0)),
                        format!("ratio={:.2}%", ratio * 100.0),
                    ],
                })
            } else {
                None
            }
        })
        .collect()
}

/// Detect deployment-related error spikes from historical events.
#[must_use]
pub fn detect_deployment_errors(events: &[DockerEvent], threshold: u64) -> Vec<Anomaly> {
    // Count "die" events per container.
    let mut die_counts: HashMap<String, u64> = HashMap::new();
    let mut die_times: HashMap<String, Vec<String>> = HashMap::new();
    for event in events {
        if event.action == "die" {
            let container = event
                .container
                .as_deref()
                .unwrap_or(&event.actor_id)
                .to_owned();
            *die_counts.entry(container.clone()).or_default() += 1;
            die_times
                .entry(container)
                .or_default()
                .push(event.time.clone());
        }
    }

    die_counts
        .into_iter()
        .filter(|(_, count)| *count >= threshold)
        .map(|(container, count)| {
            let times = die_times.get(&container).cloned().unwrap_or_default();
            let severity = if count >= threshold * 2 {
                Severity::Critical
            } else {
                Severity::Warning
            };
            Anomaly {
                severity,
                kind: "deployment_errors".to_owned(),
                container: container.clone(),
                message: format!(
                    "Container '{container}' has died {count} times (threshold: {threshold})"
                ),
                evidence: times.into_iter().map(|t| format!("die_at={t}")).collect(),
            }
        })
        .collect()
}

/// Detect containers in unhealthy states (exited, dead, restarting).
#[must_use]
pub fn detect_unhealthy_states(containers: &[Container]) -> Vec<Anomaly> {
    containers
        .iter()
        .filter_map(|c| {
            let (severity, kind) = match c.state.as_str() {
                "exited" => (Severity::Warning, "exited_container"),
                "dead" => (Severity::Critical, "dead_container"),
                "restarting" => (Severity::Warning, "restarting_container"),
                _ => return None,
            };
            Some(Anomaly {
                severity,
                kind: kind.to_owned(),
                container: c.name.clone(),
                message: format!(
                    "Container '{}' is in '{}' state: {}",
                    c.name, c.state, c.status
                ),
                evidence: vec![
                    format!("state={}", c.state),
                    format!("status={}", c.status),
                    format!("image={}", c.image),
                ],
            })
        })
        .collect()
}

// ── New analysis: Resource Leak Detection ────────────────────────────

/// Detect containers whose memory usage is trending upward over time,
/// indicating a possible memory leak.
pub fn detect_resource_leaks<S>(store: &S, thresholds: &AnalysisThresholds) -> Vec<Anomaly>
where
    S: TelemetryStore + ?Sized,
{
    let Ok(samples) = store.latest_metrics() else { return Vec::new(); };

    // Group samples by container ID and sort by timestamp.
    let mut by_container: std::collections::BTreeMap<String, Vec<&MetricSample>> =
        std::collections::BTreeMap::new();
    for sample in &samples {
        by_container
            .entry(sample.container_id.clone())
            .or_default()
            .push(sample);
    }

    let mut anomalies = Vec::new();

    // Build a reverse lookup from container_id -> name for display.
    let id_to_name: std::collections::HashMap<String, String> = samples
        .iter()
        .map(|s| (s.container_id.clone(), s.container_name.clone()))
        .collect();

    for (container_id, container_samples) in &by_container {
        let container_name = id_to_name
            .get(container_id)
            .map_or(container_id.as_str(), std::string::String::as_str);
        if container_samples.len() < thresholds.resource_leak_min_samples as usize {
            continue;
        }

        // Sort by timestamp (chronological order).
        let mut sorted = container_samples.clone();
        sorted.sort_by_key(|s| s.timestamp.as_str());

        // Collect memory readings.
        let mem_values: Vec<u64> = sorted.iter().filter_map(|s| s.memory_usage_bytes).collect();

        if mem_values.len() < 2 {
            continue;
        }

        let first = mem_values[0];
        let last = mem_values[mem_values.len() - 1];

        if first == 0 {
            continue;
        }

        let increase_pct = ((last as f64 - first as f64) / first as f64) * 100.0;

        if increase_pct >= thresholds.resource_leak_memory_increase_pct {
            let severity = if increase_pct >= thresholds.resource_leak_memory_increase_pct * 2.0 {
                Severity::Critical
            } else {
                Severity::Warning
            };

            anomalies.push(Anomaly {
                severity,
                kind: "resource_leak".to_owned(),
                container: container_name.to_string(),
                message: format!(
                    "Container '{}' memory grew {:.0}% (from {} to {}) over {} samples — possible leak",
                    container_name,
                    increase_pct,
                    format_bytes(first),
                    format_bytes(last),
                    mem_values.len()
                ),
                evidence: vec![
                    format!("initial_memory={}", first),
                    format!("latest_memory={}", last),
                    format!("increase_pct={:.1}", increase_pct),
                    format!("samples={}", mem_values.len()),
                ],
            });
        }
    }

    anomalies
}

// ── New analysis: Dependency Analysis ─────────────────────────────────

/// Analyze container dependencies: which containers share networks,
/// compose projects, and images.
pub async fn analyze_dependencies<C>(docker: &C) -> Result<ExecutionResult, AnalyzeError>
where
    C: DockerClient + ?Sized,
{
    let containers = docker.list_containers().await?;
    let networks = docker.list_networks().await.unwrap_or_default();
    let volumes = docker.list_volumes().await.unwrap_or_default();

    let mut rows = Vec::new();

    // 1. Compose project groupings (from labels).
    let mut by_compose: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for c in &containers {
        let project = c
            .labels
            .iter()
            .find_map(|label| {
                let parts: Vec<&str> = label.splitn(2, '=').collect();
                if parts.len() == 2 && parts[0] == "com.docker.compose.project" {
                    Some(parts[1].to_owned())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "standalone".to_owned());
        by_compose.entry(project).or_default().push(c.name.clone());
    }

    for (project, names) in &by_compose {
        if names.len() > 1 || project != "standalone" {
            rows.push(Row {
                fields: BTreeMap::from([
                    (
                        "dependency".to_owned(),
                        JsonValue::String("compose_project".to_owned()),
                    ),
                    ("key".to_owned(), JsonValue::String(project.clone())),
                    (
                        "containers".to_owned(),
                        JsonValue::Array(
                            names.iter().map(|n| JsonValue::String(n.clone())).collect(),
                        ),
                    ),
                    (
                        "count".to_owned(),
                        JsonValue::Number(Number::from(names.len())),
                    ),
                ]),
            });
        }
    }

    // 2. Network dependencies.
    for n in &networks {
        if !n.containers.is_empty() {
            rows.push(Row {
                fields: BTreeMap::from([
                    (
                        "dependency".to_owned(),
                        JsonValue::String("network".to_owned()),
                    ),
                    ("key".to_owned(), JsonValue::String(n.name.clone())),
                    (
                        "containers".to_owned(),
                        JsonValue::Array(
                            n.containers
                                .iter()
                                .map(|c| JsonValue::String(c.clone()))
                                .collect(),
                        ),
                    ),
                    (
                        "count".to_owned(),
                        JsonValue::Number(Number::from(n.containers.len())),
                    ),
                ]),
            });
        }
    }

    // 3. Volume dependencies (by compose project labels).
    // Volumes don't have container lists, so we note them by driver.
    for v in &volumes {
        rows.push(Row {
            fields: BTreeMap::from([
                (
                    "dependency".to_owned(),
                    JsonValue::String("volume".to_owned()),
                ),
                ("key".to_owned(), JsonValue::String(v.name.clone())),
                ("containers".to_owned(), JsonValue::Array(Vec::new())),
                ("count".to_owned(), JsonValue::Number(Number::from(0))),
            ]),
        });
    }

    if rows.is_empty() {
        rows.push(Row {
            fields: BTreeMap::from([
                (
                    "dependency".to_owned(),
                    JsonValue::String("none".to_owned()),
                ),
                ("key".to_owned(), JsonValue::String("-".to_owned())),
                ("containers".to_owned(), JsonValue::Array(Vec::new())),
                ("count".to_owned(), JsonValue::Number(Number::from(0))),
            ]),
        });
    }

    Ok(ExecutionResult { rows })
}

// ── New analysis: Config Drift Detection ───────────────────────────────

/// Detect configuration drift between historical snapshots.
///
/// Compares the two most recent snapshots in the store and emits anomalies
/// for containers whose image, state, or labels have changed.
pub fn detect_config_drift<S>(store: &S) -> Result<Vec<Anomaly>, AnalyzeError>
where
    S: TelemetryStore + ?Sized,
{
    // Get all snapshots to find the two most recent.
    let mut all = store
        .all_snapshots()
        .map_err(|_| AnalyzeError::Unsupported)?;

    if all.is_empty() {
        return Err(AnalyzeError::Unsupported);
    }

    // Snapshots are sorted by timestamp ascending.
    let latest = all.pop().expect("at least one snapshot after empty check");
    let previous = all.pop();

    // If there's only one snapshot, emit baselines.
    // Otherwise, compare the two most recent.

    let mut anomalies = Vec::new();

    match previous {
        Some(prev) => {
            // We have two snapshots — compare them.
            for container in &latest.containers {
                let prev_container = prev.containers.iter().find(|c| c.name == container.name);

                match prev_container {
                    Some(prev_c) => {
                        // Check for image drift.
                        if prev_c.image != container.image {
                            anomalies.push(Anomaly {
                                severity: Severity::Warning,
                                kind: "config_drift".to_owned(),
                                container: container.name.clone(),
                                message: format!(
                                    "Container '{}' image changed from '{}' to '{}'",
                                    container.name, prev_c.image, container.image
                                ),
                                evidence: vec![
                                    format!("previous_image={}", prev_c.image),
                                    format!("current_image={}", container.image),
                                ],
                            });
                        }

                        // Check for state drift.
                        if prev_c.state != container.state {
                            let severity = match container.state.as_str() {
                                "running" => Severity::Info,
                                _ => Severity::Warning,
                            };
                            anomalies.push(Anomaly {
                                severity,
                                kind: "state_change".to_owned(),
                                container: container.name.clone(),
                                message: format!(
                                    "Container '{}' state changed from '{}' to '{}'",
                                    container.name, prev_c.state, container.state
                                ),
                                evidence: vec![
                                    format!("previous_state={}", prev_c.state),
                                    format!("current_state={}", container.state),
                                ],
                            });
                        }

                        // Check for restart count drift.
                        let prev_restarts = prev_c.restart_count.unwrap_or(0);
                        let curr_restarts = container.restart_count.unwrap_or(0);
                        if curr_restarts > prev_restarts {
                            anomalies.push(Anomaly {
                                severity: Severity::Warning,
                                kind: "restart_increase".to_owned(),
                                container: container.name.clone(),
                                message: format!(
                                    "Container '{}' restart count increased from {} to {}",
                                    container.name, prev_restarts, curr_restarts
                                ),
                                evidence: vec![
                                    format!("previous_restarts={}", prev_restarts),
                                    format!("current_restarts={}", curr_restarts),
                                ],
                            });
                        }

                        // Check for label drift.
                        let prev_labels: std::collections::BTreeSet<&str> =
                            prev_c.labels.iter().map(std::string::String::as_str).collect();
                        let curr_labels: std::collections::BTreeSet<&str> =
                            container.labels.iter().map(std::string::String::as_str).collect();
                        let added: Vec<&&str> = curr_labels.difference(&prev_labels).collect();
                        let removed: Vec<&&str> = prev_labels.difference(&curr_labels).collect();
                        if !added.is_empty() || !removed.is_empty() {
                            anomalies.push(Anomaly {
                                severity: Severity::Info,
                                kind: "label_drift".to_owned(),
                                container: container.name.clone(),
                                message: format!(
                                    "Container '{}' labels changed: added={:?}, removed={:?}",
                                    container.name, added, removed
                                ),
                                evidence: vec![
                                    format!("added_labels={:?}", added),
                                    format!("removed_labels={:?}", removed),
                                ],
                            });
                        }
                    }
                    None => {
                        // New container appeared.
                        anomalies.push(Anomaly {
                            severity: Severity::Info,
                            kind: "container_appeared".to_owned(),
                            container: container.name.clone(),
                            message: format!(
                                "Container '{}' appeared with image={}, state={}",
                                container.name, container.image, container.state
                            ),
                            evidence: vec![
                                format!("image={}", container.image),
                                format!("state={}", container.state),
                            ],
                        });
                    }
                }
            }

            // Check for containers that disappeared.
            for prev_c in &prev.containers {
                if !latest.containers.iter().any(|c| c.name == prev_c.name) {
                    anomalies.push(Anomaly {
                        severity: Severity::Warning,
                        kind: "container_disappeared".to_owned(),
                        container: prev_c.name.clone(),
                        message: format!(
                            "Container '{}' was present in previous snapshot but is now gone",
                            prev_c.name
                        ),
                        evidence: vec![
                            format!("previous_state={}", prev_c.state),
                            format!("previous_image={}", prev_c.image),
                        ],
                    });
                }
            }
        }
        None => {
            // Only one snapshot — report it as baseline.
            for container in &latest.containers {
                anomalies.push(Anomaly {
                    severity: Severity::Info,
                    kind: "config_baseline".to_owned(),
                    container: container.name.clone(),
                    message: format!(
                        "Container '{}' baseline — image={}, state={}",
                        container.name, container.image, container.state
                    ),
                    evidence: vec![
                        format!("image={}", container.image),
                        format!("state={}", container.state),
                        format!("restart_count={}", container.restart_count.unwrap_or(0)),
                    ],
                });
            }
        }
    }

    Ok(anomalies)
}

// ── New analysis: Density Analysis ─────────────────────────────────────

/// Analyze container density / distribution across images, states,
/// and compose projects.
pub async fn analyze_density<C>(docker: &C) -> Result<ExecutionResult, AnalyzeError>
where
    C: DockerClient + ?Sized,
{
    let containers = docker.list_containers().await?;
    let total = containers.len();
    let mut rows = Vec::new();

    // 1. By image.
    let mut by_image: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for c in &containers {
        by_image
            .entry(c.image.clone())
            .or_default()
            .push(c.name.clone());
    }
    for (image, names) in &by_image {
        let pct = if total > 0 {
            (names.len() as f64 / total as f64) * 100.0
        } else {
            0.0
        };
        rows.push(Row {
            fields: BTreeMap::from([
                (
                    "dimension".to_owned(),
                    JsonValue::String("image".to_owned()),
                ),
                ("value".to_owned(), JsonValue::String(image.clone())),
                (
                    "container_count".to_owned(),
                    JsonValue::Number(Number::from(names.len())),
                ),
                (
                    "density_pct".to_owned(),
                    JsonValue::Number(
                        Number::from_f64((pct * 10.0).round() / 10.0).unwrap_or_else(|| Number::from(0)),
                    ),
                ),
            ]),
        });
    }

    // 2. By state.
    let mut by_state: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for c in &containers {
        *by_state.entry(c.state.clone()).or_default() += 1;
    }
    for (state, count) in &by_state {
        let pct = if total > 0 {
            (*count as f64 / total as f64) * 100.0
        } else {
            0.0
        };
        rows.push(Row {
            fields: BTreeMap::from([
                (
                    "dimension".to_owned(),
                    JsonValue::String("state".to_owned()),
                ),
                ("value".to_owned(), JsonValue::String(state.clone())),
                (
                    "container_count".to_owned(),
                    JsonValue::Number(Number::from(*count)),
                ),
                (
                    "density_pct".to_owned(),
                    JsonValue::Number(
                        Number::from_f64((pct * 10.0).round() / 10.0).unwrap_or_else(|| Number::from(0)),
                    ),
                ),
            ]),
        });
    }

    // 3. By compose project.
    let mut by_project: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for c in &containers {
        let project = c
            .labels
            .iter()
            .find_map(|label| {
                let parts: Vec<&str> = label.splitn(2, '=').collect();
                if parts.len() == 2 && parts[0] == "com.docker.compose.project" {
                    Some(parts[1].to_owned())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "standalone".to_owned());
        by_project.entry(project).or_default().push(c.name.clone());
    }
    for (project, names) in &by_project {
        let pct = if total > 0 {
            (names.len() as f64 / total as f64) * 100.0
        } else {
            0.0
        };
        rows.push(Row {
            fields: BTreeMap::from([
                (
                    "dimension".to_owned(),
                    JsonValue::String("compose_project".to_owned()),
                ),
                ("value".to_owned(), JsonValue::String(project.clone())),
                (
                    "container_count".to_owned(),
                    JsonValue::Number(Number::from(names.len())),
                ),
                (
                    "density_pct".to_owned(),
                    JsonValue::Number(
                        Number::from_f64((pct * 10.0).round() / 10.0).unwrap_or_else(|| Number::from(0)),
                    ),
                ),
            ]),
        });
    }

    // Total summary.
    rows.push(Row {
        fields: BTreeMap::from([
            (
                "dimension".to_owned(),
                JsonValue::String("total".to_owned()),
            ),
            ("value".to_owned(), JsonValue::String("-".to_owned())),
            (
                "container_count".to_owned(),
                JsonValue::Number(Number::from(total)),
            ),
            (
                "density_pct".to_owned(),
                JsonValue::Number(Number::from(100)),
            ),
        ]),
    });

    Ok(ExecutionResult { rows })
}

// ── Composite analysis functions ───────────────────────────────────────────

/// `analyze containers find anomalies` — runs all detectors on live data.
async fn find_container_anomalies<C, M>(
    docker: &C,
    metrics: &M,
    thresholds: &AnalysisThresholds,
) -> Result<ExecutionResult, AnalyzeError>
where
    C: DockerClient + ?Sized,
    M: MetricsCollector + ?Sized,
{
    let containers = docker.list_containers().await?;
    let samples = metrics.collect().await?;

    let mut anomalies = Vec::new();
    anomalies.extend(detect_restart_loops(
        &containers,
        thresholds.restart_loop_count,
    ));
    anomalies.extend(detect_high_cpu(
        &samples,
        thresholds.high_cpu_percent,
        thresholds.critical_cpu_percent,
    ));
    anomalies.extend(detect_memory_pressure(
        &samples,
        thresholds.memory_pressure_ratio,
        thresholds.critical_memory_ratio,
    ));
    anomalies.extend(detect_unhealthy_states(&containers));

    // Sort by severity (Critical first).
    anomalies.sort_by_key(|a| std::cmp::Reverse(severity_rank(a.severity)));

    if anomalies.is_empty() {
        Ok(ExecutionResult {
            rows: vec![Row {
                fields: BTreeMap::from([
                    ("severity".to_owned(), JsonValue::String("info".to_owned())),
                    ("kind".to_owned(), JsonValue::String("healthy".to_owned())),
                    ("container".to_owned(), JsonValue::String("*".to_owned())),
                    (
                        "message".to_owned(),
                        JsonValue::String("No anomalies detected".to_owned()),
                    ),
                    ("evidence".to_owned(), JsonValue::Array(Vec::new())),
                ]),
            }],
        })
    } else {
        Ok(ExecutionResult {
            rows: anomalies.iter().map(Anomaly::to_row).collect(),
        })
    }
}

/// `analyze containers find anomalies` using historical store data.
fn find_anomalies_from_store<S>(
    store: &S,
    thresholds: &AnalysisThresholds,
) -> Result<ExecutionResult, AnalyzeError>
where
    S: TelemetryStore + ?Sized,
{
    let samples = store.latest_metrics()?;
    let events = store.events_between("", "\u{10ffff}")?;

    let mut anomalies = Vec::new();
    anomalies.extend(detect_high_cpu(
        &samples,
        thresholds.high_cpu_percent,
        thresholds.critical_cpu_percent,
    ));
    anomalies.extend(detect_memory_pressure(
        &samples,
        thresholds.memory_pressure_ratio,
        thresholds.critical_memory_ratio,
    ));
    anomalies.extend(detect_deployment_errors(
        &events,
        thresholds.deployment_error_threshold,
    ));

    anomalies.sort_by_key(|a| std::cmp::Reverse(severity_rank(a.severity)));

    if anomalies.is_empty() {
        Ok(ExecutionResult {
            rows: vec![Row {
                fields: BTreeMap::from([
                    ("severity".to_owned(), JsonValue::String("info".to_owned())),
                    ("kind".to_owned(), JsonValue::String("healthy".to_owned())),
                    ("container".to_owned(), JsonValue::String("*".to_owned())),
                    (
                        "message".to_owned(),
                        JsonValue::String("No anomalies detected in stored data".to_owned()),
                    ),
                    ("evidence".to_owned(), JsonValue::Array(Vec::new())),
                ]),
            }],
        })
    } else {
        Ok(ExecutionResult {
            rows: anomalies.iter().map(Anomaly::to_row).collect(),
        })
    }
}

/// `analyze containers correlate` — find containers sharing images/networks.
async fn correlate_containers<C, M>(
    docker: &C,
    _metrics: &M,
    _thresholds: &AnalysisThresholds,
) -> Result<ExecutionResult, AnalyzeError>
where
    C: DockerClient + ?Sized,
    M: MetricsCollector + ?Sized,
{
    let containers = docker.list_containers().await?;

    // Group containers by image.
    let mut by_image: HashMap<String, Vec<String>> = HashMap::new();
    for c in &containers {
        by_image
            .entry(c.image.clone())
            .or_default()
            .push(c.name.clone());
    }

    let mut rows = Vec::new();

    for (image, names) in &by_image {
        if names.len() > 1 {
            rows.push(Row {
                fields: BTreeMap::from([
                    (
                        "correlation".to_owned(),
                        JsonValue::String("shared_image".to_owned()),
                    ),
                    ("key".to_owned(), JsonValue::String(image.clone())),
                    (
                        "containers".to_owned(),
                        JsonValue::Array(
                            names.iter().map(|n| JsonValue::String(n.clone())).collect(),
                        ),
                    ),
                    (
                        "count".to_owned(),
                        JsonValue::Number(Number::from(names.len())),
                    ),
                ]),
            });
        }
    }

    // Group by shared labels.
    let mut by_label: HashMap<String, Vec<String>> = HashMap::new();
    for c in &containers {
        for label in &c.labels {
            by_label
                .entry(label.clone())
                .or_default()
                .push(c.name.clone());
        }
    }

    for (label, names) in &by_label {
        if names.len() > 1 {
            rows.push(Row {
                fields: BTreeMap::from([
                    (
                        "correlation".to_owned(),
                        JsonValue::String("shared_label".to_owned()),
                    ),
                    ("key".to_owned(), JsonValue::String(label.clone())),
                    (
                        "containers".to_owned(),
                        JsonValue::Array(
                            names.iter().map(|n| JsonValue::String(n.clone())).collect(),
                        ),
                    ),
                    (
                        "count".to_owned(),
                        JsonValue::Number(Number::from(names.len())),
                    ),
                ]),
            });
        }
    }

    if rows.is_empty() {
        rows.push(Row {
            fields: BTreeMap::from([
                (
                    "correlation".to_owned(),
                    JsonValue::String("none".to_owned()),
                ),
                ("key".to_owned(), JsonValue::String("-".to_owned())),
                ("containers".to_owned(), JsonValue::Array(Vec::new())),
                ("count".to_owned(), JsonValue::Number(Number::from(0))),
            ]),
        });
    }

    Ok(ExecutionResult { rows })
}

/// `explain container <name>` — produce a diagnostic summary for one container.
async fn explain_container<C, M>(
    name: &str,
    docker: &C,
    metrics: &M,
    thresholds: &AnalysisThresholds,
) -> Result<ExecutionResult, AnalyzeError>
where
    C: DockerClient + ?Sized,
    M: MetricsCollector + ?Sized,
{
    let containers = docker.list_containers().await?;
    let container = containers
        .iter()
        .find(|c| c.name == name || c.id == name)
        .ok_or(AnalyzeError::Unsupported)?;

    let samples = metrics.collect().await?;
    let sample = samples
        .iter()
        .find(|s| s.container_name == name || s.container_id == name);

    let explanation = build_explanation(container, sample, thresholds);

    Ok(explanation_to_result(&explanation))
}

/// `analyze containers explain` — produce diagnostic summaries for all containers.
async fn explain_all_containers<C, M>(
    docker: &C,
    metrics: &M,
    thresholds: &AnalysisThresholds,
) -> Result<ExecutionResult, AnalyzeError>
where
    C: DockerClient + ?Sized,
    M: MetricsCollector + ?Sized,
{
    let containers = docker.list_containers().await?;
    let samples = metrics.collect().await?;
    let samples_by_name: HashMap<&str, &MetricSample> = samples
        .iter()
        .map(|s| (s.container_name.as_str(), s))
        .collect();

    let mut rows = Vec::new();
    for container in &containers {
        let sample = samples_by_name.get(container.name.as_str()).copied();
        let explanation = build_explanation(container, sample, thresholds);
        let result = explanation_to_result(&explanation);
        rows.extend(result.rows);
    }

    if rows.is_empty() {
        rows.push(Row {
            fields: BTreeMap::from([
                ("container".to_owned(), JsonValue::String("-".to_owned())),
                ("state".to_owned(), JsonValue::String("-".to_owned())),
                ("signal".to_owned(), JsonValue::String("none".to_owned())),
                ("value".to_owned(), JsonValue::String("-".to_owned())),
                ("status".to_owned(), JsonValue::String("normal".to_owned())),
                (
                    "anomaly_count".to_owned(),
                    JsonValue::Number(Number::from(0)),
                ),
            ]),
        });
    }

    Ok(ExecutionResult { rows })
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn build_explanation(
    container: &Container,
    sample: Option<&MetricSample>,
    thresholds: &AnalysisThresholds,
) -> ContainerExplanation {
    let mut signals = Vec::new();
    let mut anomalies = Vec::new();

    // State signal.
    let state_status = match container.state.as_str() {
        "running" => SignalStatus::Normal,
        "restarting" => SignalStatus::Elevated,
        _ => SignalStatus::Critical,
    };
    signals.push(Signal {
        name: "state".to_owned(),
        value: container.state.clone(),
        status: state_status,
    });

    // Restart count signal.
    if let Some(count) = container.restart_count {
        let status = if count >= thresholds.restart_loop_count * 2 {
            SignalStatus::Critical
        } else if count >= thresholds.restart_loop_count {
            SignalStatus::Elevated
        } else {
            SignalStatus::Normal
        };
        signals.push(Signal {
            name: "restart_count".to_owned(),
            value: count.to_string(),
            status,
        });

        if count >= thresholds.restart_loop_count {
            anomalies.extend(detect_restart_loops(
                std::slice::from_ref(container),
                thresholds.restart_loop_count,
            ));
        }
    }

    // Metric signals.
    if let Some(s) = sample {
        if let Some(cpu) = s.cpu_percent {
            let status = if cpu >= thresholds.critical_cpu_percent {
                SignalStatus::Critical
            } else if cpu >= thresholds.high_cpu_percent {
                SignalStatus::Elevated
            } else {
                SignalStatus::Normal
            };
            signals.push(Signal {
                name: "cpu".to_owned(),
                value: format!("{cpu:.1}%"),
                status,
            });
        }

        if let (Some(usage), Some(limit)) = (s.memory_usage_bytes, s.memory_limit_bytes) {
            let ratio = if limit > 0 {
                usage as f64 / limit as f64
            } else {
                0.0
            };
            let status = if ratio >= thresholds.critical_memory_ratio {
                SignalStatus::Critical
            } else if ratio >= thresholds.memory_pressure_ratio {
                SignalStatus::Elevated
            } else {
                SignalStatus::Normal
            };
            signals.push(Signal {
                name: "memory".to_owned(),
                value: format!("{:.1}%", ratio * 100.0),
                status,
            });
        }

        if let Some(rx) = s.network_rx_bytes {
            signals.push(Signal {
                name: "network_rx".to_owned(),
                value: format_bytes(rx),
                status: SignalStatus::Normal,
            });
        }

        if let Some(tx) = s.network_tx_bytes {
            signals.push(Signal {
                name: "network_tx".to_owned(),
                value: format_bytes(tx),
                status: SignalStatus::Normal,
            });
        }

        // Also run metric detectors for anomalies.
        anomalies.extend(detect_high_cpu(
            std::slice::from_ref(s),
            thresholds.high_cpu_percent,
            thresholds.critical_cpu_percent,
        ));
        anomalies.extend(detect_memory_pressure(
            std::slice::from_ref(s),
            thresholds.memory_pressure_ratio,
            thresholds.critical_memory_ratio,
        ));
    }

    // Unhealthy state.
    anomalies.extend(detect_unhealthy_states(std::slice::from_ref(container)));

    ContainerExplanation {
        container: container.name.clone(),
        state: container.state.clone(),
        anomalies,
        signals,
    }
}

fn explanation_to_result(explanation: &ContainerExplanation) -> ExecutionResult {
    let anomaly_count = explanation.anomalies.len();
    let rows = explanation
        .signals
        .iter()
        .map(|signal| Row {
            fields: BTreeMap::from([
                (
                    "container".to_owned(),
                    JsonValue::String(explanation.container.clone()),
                ),
                (
                    "state".to_owned(),
                    JsonValue::String(explanation.state.clone()),
                ),
                ("signal".to_owned(), JsonValue::String(signal.name.clone())),
                ("value".to_owned(), JsonValue::String(signal.value.clone())),
                (
                    "status".to_owned(),
                    JsonValue::String(signal.status.to_string()),
                ),
                (
                    "anomaly_count".to_owned(),
                    JsonValue::Number(Number::from(anomaly_count)),
                ),
            ]),
        })
        .collect();

    ExecutionResult { rows }
}

const fn severity_rank(severity: Severity) -> u8 {
    match severity {
        Severity::Info => 0,
        Severity::Warning => 1,
        Severity::Critical => 2,
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= ONE_GB {
        format!("{:.1} GB", bytes as f64 / ONE_GB as f64)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::Container;

    fn rt() -> tokio::runtime::Runtime { tokio::runtime::Runtime::new().unwrap() }

    fn test_container(name: &str, state: &str, restarts: u64) -> Container {
        Container {
            id: format!("{name}_id"),
            name: name.to_owned(),
            image: format!("{name}:latest"),
            status: if state == "running" {
                "Up 2 minutes".to_owned()
            } else {
                "Exited (1) 1 minute ago".to_string()
            },
            state: state.to_owned(),
            ports: Vec::new(),
            labels: vec!["env=prod".to_owned()],
            created_at: None,
            started_at: None,
            finished_at: None,
            restart_count: Some(restarts),
            health: None,
        }
    }

    fn test_sample(name: &str, cpu: f64, mem_usage: u64, mem_limit: u64) -> MetricSample {
        MetricSample {
            container_id: format!("{name}_id"),
            container_name: name.to_owned(),
            timestamp: "2026-01-01T12:00:00Z".to_owned(),
            cpu_percent: Some(cpu),
            memory_usage_bytes: Some(mem_usage),
            memory_limit_bytes: Some(mem_limit),
            network_rx_bytes: Some(1024),
            network_tx_bytes: Some(2048),
            disk_read_bytes: Some(0),
            disk_write_bytes: Some(0),
        }
    }

    fn test_event(time: &str, action: &str, container: &str) -> DockerEvent {
        DockerEvent {
            time: time.to_owned(),
            event_type: "container".to_owned(),
            action: action.to_owned(),
            actor_id: format!("{container}_id"),
            container: Some(container.to_owned()),
            image: Some(format!("{container}:latest")),
            attributes: vec![("name".to_owned(), container.to_owned())],
        }
    }

    // ── Restart loop tests ─────────────────────────────────────────────

    #[test]
    fn detects_restart_loop() {
        let containers = vec![
            test_container("api", "running", 5),
            test_container("worker", "running", 0),
        ];

        let anomalies = detect_restart_loops(&containers, 3);

        assert_eq!(anomalies.len(), 1);
        assert_eq!(anomalies[0].container, "api");
        assert_eq!(anomalies[0].kind, "restart_loop");
        assert_eq!(anomalies[0].severity, Severity::Warning);
    }

    #[test]
    fn detects_critical_restart_loop() {
        let containers = vec![test_container("api", "running", 10)];
        let anomalies = detect_restart_loops(&containers, 3);

        assert_eq!(anomalies.len(), 1);
        assert_eq!(anomalies[0].severity, Severity::Critical);
    }

    #[test]
    fn no_restart_loop_below_threshold() {
        let containers = vec![test_container("api", "running", 2)];
        let anomalies = detect_restart_loops(&containers, 3);

        assert!(anomalies.is_empty());
    }

    // ── High CPU tests ─────────────────────────────────────────────────

    #[test]
    fn detects_high_cpu_warning() {
        let samples = vec![test_sample("api", 85.0, 512, 1024)];
        let anomalies = detect_high_cpu(&samples, 80.0, 95.0);

        assert_eq!(anomalies.len(), 1);
        assert_eq!(anomalies[0].severity, Severity::Warning);
        assert_eq!(anomalies[0].kind, "high_cpu");
    }

    #[test]
    fn detects_critical_cpu() {
        let samples = vec![test_sample("api", 98.0, 512, 1024)];
        let anomalies = detect_high_cpu(&samples, 80.0, 95.0);

        assert_eq!(anomalies.len(), 1);
        assert_eq!(anomalies[0].severity, Severity::Critical);
    }

    #[test]
    fn no_cpu_anomaly_below_threshold() {
        let samples = vec![test_sample("api", 50.0, 512, 1024)];
        let anomalies = detect_high_cpu(&samples, 80.0, 95.0);

        assert!(anomalies.is_empty());
    }

    // ── Memory pressure tests ──────────────────────────────────────────

    #[test]
    fn detects_memory_pressure_warning() {
        let samples = vec![test_sample("api", 10.0, 900, 1024)]; // ~87.9%
        let anomalies = detect_memory_pressure(&samples, 0.85, 0.95);

        assert_eq!(anomalies.len(), 1);
        assert_eq!(anomalies[0].severity, Severity::Warning);
        assert_eq!(anomalies[0].kind, "memory_pressure");
    }

    #[test]
    fn detects_critical_memory_pressure() {
        let samples = vec![test_sample("api", 10.0, 980, 1024)]; // ~95.7%
        let anomalies = detect_memory_pressure(&samples, 0.85, 0.95);

        assert_eq!(anomalies.len(), 1);
        assert_eq!(anomalies[0].severity, Severity::Critical);
    }

    #[test]
    fn no_memory_pressure_below_threshold() {
        let samples = vec![test_sample("api", 10.0, 500, 1024)]; // ~48.8%
        let anomalies = detect_memory_pressure(&samples, 0.85, 0.95);

        assert!(anomalies.is_empty());
    }

    // ── Deployment errors tests ────────────────────────────────────────

    #[test]
    fn detects_deployment_errors() {
        let events = vec![
            test_event("2026-01-01T12:00:00Z", "die", "api"),
            test_event("2026-01-01T12:01:00Z", "die", "api"),
            test_event("2026-01-01T12:02:00Z", "die", "api"),
            test_event("2026-01-01T12:03:00Z", "start", "api"),
        ];

        let anomalies = detect_deployment_errors(&events, 3);

        assert_eq!(anomalies.len(), 1);
        assert_eq!(anomalies[0].container, "api");
        assert_eq!(anomalies[0].kind, "deployment_errors");
    }

    #[test]
    fn no_deployment_errors_below_threshold() {
        let events = vec![
            test_event("2026-01-01T12:00:00Z", "die", "api"),
            test_event("2026-01-01T12:01:00Z", "start", "api"),
        ];

        let anomalies = detect_deployment_errors(&events, 3);

        assert!(anomalies.is_empty());
    }

    // ── Unhealthy state tests ──────────────────────────────────────────

    #[test]
    fn detects_unhealthy_states() {
        let containers = vec![
            test_container("api", "running", 0),
            test_container("worker", "exited", 0),
            test_container("db", "dead", 0),
        ];

        let anomalies = detect_unhealthy_states(&containers);

        assert_eq!(anomalies.len(), 2);
        let kinds: Vec<&str> = anomalies.iter().map(|a| a.kind.as_str()).collect();
        assert!(kinds.contains(&"exited_container"));
        assert!(kinds.contains(&"dead_container"));
    }

    // ── Integration tests ──────────────────────────────────────────────

    #[test]
    fn find_anomalies_returns_multiple_types() {
        let docker = crate::docker::MockDockerClient {
            containers: vec![
                test_container("api", "running", 5),
                test_container("worker", "exited", 0),
            ],
            ..Default::default()
        };
        let metrics = crate::metrics::MockMetricsCollector {
            samples: vec![test_sample("api", 92.0, 950, 1024)],
        };

        let query = AnalyzeQuery {
            target: AnalysisTarget::Collection(CollectionTarget::Containers),
            verb: AnalysisVerb::Find,
            subject: Some("anomalies".to_owned()),
            time: None,
            pipeline: Vec::new(),
        };

        let result = rt().block_on(execute_analyze(&query, &docker, &metrics)).unwrap();

        // Should find: restart_loop + high_cpu + memory_pressure + exited_container
        assert!(result.rows.len() >= 3);
        let kinds: Vec<String> = result
            .rows
            .iter()
            .filter_map(|r| {
                r.fields
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            })
            .collect();
        assert!(kinds.contains(&"restart_loop".to_owned()));
        assert!(kinds.contains(&"high_cpu".to_owned()));
        assert!(kinds.contains(&"exited_container".to_owned()));
    }

    #[test]
    fn find_anomalies_returns_healthy_when_no_issues() {
        let docker = crate::docker::MockDockerClient {
            containers: vec![test_container("api", "running", 0)],
            ..Default::default()
        };
        let metrics = crate::metrics::MockMetricsCollector {
            samples: vec![test_sample("api", 10.0, 128, 1024)],
        };

        let query = AnalyzeQuery {
            target: AnalysisTarget::Collection(CollectionTarget::Containers),
            verb: AnalysisVerb::Find,
            subject: Some("anomalies".to_owned()),
            time: None,
            pipeline: Vec::new(),
        };

        let result = rt().block_on(execute_analyze(&query, &docker, &metrics)).unwrap();

        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["kind"],
            JsonValue::String("healthy".to_owned())
        );
    }

    #[test]
    fn explain_container_produces_signals() {
        let docker = crate::docker::MockDockerClient {
            containers: vec![test_container("api", "running", 5)],
            ..Default::default()
        };
        let metrics = crate::metrics::MockMetricsCollector {
            samples: vec![test_sample("api", 85.0, 900, 1024)],
        };

        let query = AnalyzeQuery {
            target: AnalysisTarget::Singular(crate::ast::SingularTarget {
                kind: SingularTargetKind::Container,
                value: "api".to_owned(),
            }),
            verb: AnalysisVerb::Explain,
            subject: None,
            time: None,
            pipeline: Vec::new(),
        };

        let result = rt().block_on(execute_analyze(&query, &docker, &metrics)).unwrap();

        // Should have signals for state, restart_count, cpu, memory, network_rx, network_tx
        assert!(result.rows.len() >= 4);
        let signal_names: Vec<String> = result
            .rows
            .iter()
            .filter_map(|r| {
                r.fields
                    .get("signal")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            })
            .collect();
        assert!(signal_names.contains(&"state".to_owned()));
        assert!(signal_names.contains(&"cpu".to_owned()));
        assert!(signal_names.contains(&"memory".to_owned()));
    }

    #[test]
    fn correlate_finds_shared_images() {
        // Create containers with the same image to test correlation.
        // so create containers with same image.
        let docker = crate::docker::MockDockerClient {
            containers: vec![
                Container {
                    image: "api:latest".to_owned(),
                    ..test_container("api-1", "running", 0)
                },
                Container {
                    image: "api:latest".to_owned(),
                    ..test_container("api-2", "running", 0)
                },
            ],
            ..Default::default()
        };
        let metrics = crate::metrics::MockMetricsCollector {
            samples: Vec::new(),
        };

        let query = AnalyzeQuery {
            target: AnalysisTarget::Collection(CollectionTarget::Containers),
            verb: AnalysisVerb::Correlate,
            subject: None,
            time: None,
            pipeline: Vec::new(),
        };

        let result = rt().block_on(execute_analyze(&query, &docker, &metrics)).unwrap();

        // Should find shared_image and shared_label correlations
        let correlations: Vec<String> = result
            .rows
            .iter()
            .filter_map(|r| {
                r.fields
                    .get("correlation")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            })
            .collect();
        assert!(correlations.contains(&"shared_image".to_owned()));
    }

    // ── Resource Leak tests ────────────────────────────────────────

    #[test]
    fn detects_resource_leak_with_increasing_memory() {
        let mut store = crate::storage::InMemoryTelemetryStore::default();
        let thresholds = AnalysisThresholds {
            resource_leak_memory_increase_pct: 20.0,
            resource_leak_min_samples: 3,
            ..AnalysisThresholds::default()
        };

        // Write 5 samples with increasing memory (leak pattern)
        for i in 0..5 {
            let mem = 100_000_000 + (i * 30_000_000); // 100M, 130M, 160M, 190M, 220M
            let sample = MetricSample {                    container_id: "leaky_id".to_string(),
                    container_name: "leaky-container".to_owned(),
                timestamp: format!("2026-01-01T12:00:{:02}Z", i),
                cpu_percent: Some(50.0),
                memory_usage_bytes: Some(mem),
                memory_limit_bytes: Some(1_073_741_824),
                network_rx_bytes: Some(0),
                network_tx_bytes: Some(0),
                disk_read_bytes: Some(0),
                disk_write_bytes: Some(0),
            };
            store.write_metric(sample).unwrap();
        }

        let anomalies = detect_resource_leaks(&store, &thresholds);
        assert_eq!(anomalies.len(), 1);
        assert_eq!(anomalies[0].kind, "resource_leak");
        assert_eq!(anomalies[0].container, "leaky-container");
    }

    #[test]
    fn no_resource_leak_with_stable_memory() {
        let mut store = crate::storage::InMemoryTelemetryStore::default();
        let thresholds = AnalysisThresholds {
            resource_leak_memory_increase_pct: 20.0,
            resource_leak_min_samples: 3,
            ..AnalysisThresholds::default()
        };

        // Write 3 samples with stable memory
        for i in 0..3 {
            let sample = MetricSample {                    container_id: "stable_id".to_string(),
                    container_name: "stable".to_owned(),
                timestamp: format!("2026-01-01T12:00:{:02}Z", i),
                cpu_percent: Some(50.0),
                memory_usage_bytes: Some(100_000_000),
                memory_limit_bytes: Some(1_073_741_824),
                network_rx_bytes: Some(0),
                network_tx_bytes: Some(0),
                disk_read_bytes: Some(0),
                disk_write_bytes: Some(0),
            };
            store.write_metric(sample).unwrap();
        }

        let anomalies = detect_resource_leaks(&store, &thresholds);
        assert!(anomalies.is_empty());
    }

    #[test]
    fn no_resource_leak_with_insufficient_samples() {
        let mut store = crate::storage::InMemoryTelemetryStore::default();
        let thresholds = AnalysisThresholds {
            resource_leak_memory_increase_pct: 20.0,
            resource_leak_min_samples: 5,
            ..AnalysisThresholds::default()
        };

        // Only 2 samples — below threshold
        for i in 0..2 {
            let sample = MetricSample {                    container_id: "few_id".to_string(),
                    container_name: "few".to_owned(),
                timestamp: format!("2026-01-01T12:00:{:02}Z", i),
                cpu_percent: Some(50.0),
                memory_usage_bytes: Some(100_000_000 + (i * 50_000_000)),
                memory_limit_bytes: Some(1_073_741_824),
                network_rx_bytes: Some(0),
                network_tx_bytes: Some(0),
                disk_read_bytes: Some(0),
                disk_write_bytes: Some(0),
            };
            store.write_metric(sample).unwrap();
        }

        let anomalies = detect_resource_leaks(&store, &thresholds);
        assert!(anomalies.is_empty());
    }

    // ── Dependency tests ────────────────────────────────────────────

    #[test]
    fn finds_dependencies_from_mock() {
        let docker = crate::docker::MockDockerClient {
            containers: vec![
                Container {
                    image: "api:latest".to_owned(),
                    labels: vec!["com.docker.compose.project=myapp".to_owned()],
                    ..test_container("api-1", "running", 0)
                },
                Container {
                    image: "api:latest".to_owned(),
                    labels: vec!["com.docker.compose.project=myapp".to_owned()],
                    ..test_container("api-2", "running", 0)
                },
            ],
            networks: vec![crate::docker::Network {
                id: "net1".to_owned(),
                name: "frontend-net".to_owned(),
                driver: "bridge".to_owned(),
                scope: "local".to_owned(),
                containers: vec!["api-1".to_owned()],
                labels: Vec::new(),
            }],
            volumes: vec![crate::docker::Volume {
                name: "pgdata".to_owned(),
                driver: "local".to_owned(),
                mountpoint: Some("/data".to_owned()),
                scope: Some("local".to_owned()),
                labels: Vec::new(),
            }],
            ..Default::default()
        };

        let result = rt().block_on(analyze_dependencies(&docker)).unwrap();

        let dep_kinds: Vec<String> = result
            .rows
            .iter()
            .filter_map(|r| {
                r.fields
                    .get("dependency")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            })
            .collect();
        assert!(dep_kinds.contains(&"compose_project".to_owned()));
        assert!(dep_kinds.contains(&"network".to_owned()));
        assert!(dep_kinds.contains(&"volume".to_owned()));
    }

    #[test]
    fn dependencies_empty_returns_none_row() {
        let docker = crate::docker::MockDockerClient::default();
        let result = rt().block_on(analyze_dependencies(&docker)).unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["dependency"],
            JsonValue::String("none".to_owned())
        );
    }

    // ── Config Drift tests ──────────────────────────────────────────

    #[test]
    fn detects_config_drift_from_snapshot() {
        let mut store = crate::storage::InMemoryTelemetryStore::default();
        store
            .write_snapshot(crate::storage::TelemetrySnapshot {
                timestamp: "2026-01-01T12:00:00Z".to_owned(),
                containers: vec![Container {
                    id: "abc".to_owned(),
                    name: "api".to_owned(),
                    image: "api:latest".to_owned(),
                    status: "Up 5 minutes".to_owned(),
                    state: "running".to_owned(),
                    ports: Vec::new(),
                    labels: vec!["env=prod".to_owned()],
                    created_at: None,
                    started_at: None,
                    finished_at: None,
                    restart_count: Some(1),
                    health: None,
                }],
                images: Vec::new(),
                networks: Vec::new(),
                volumes: Vec::new(),
            })
            .unwrap();

        let anomalies = detect_config_drift(&store).unwrap();
        assert!(!anomalies.is_empty());
        assert_eq!(anomalies[0].kind, "config_baseline");
        assert_eq!(anomalies[0].container, "api");
    }

    #[test]
    fn detects_config_drift_between_snapshots() {
        let mut store = crate::storage::InMemoryTelemetryStore::default();

        // Snapshot A: api v1, running
        store
            .write_snapshot(crate::storage::TelemetrySnapshot {
                timestamp: "2026-01-01T12:00:00Z".to_owned(),
                containers: vec![Container {
                    id: "abc".to_owned(),
                    name: "api".to_owned(),
                    image: "api:v1".to_owned(),
                    status: "Up 5 minutes".to_owned(),
                    state: "running".to_owned(),
                    ports: Vec::new(),
                    labels: vec!["env=prod".to_owned(), "team=backend".to_owned()],
                    created_at: None,
                    started_at: None,
                    finished_at: None,
                    restart_count: Some(0),
                    health: None,
                }],
                images: Vec::new(),
                networks: Vec::new(),
                volumes: Vec::new(),
            })
            .unwrap();

        // Snapshot B: api v2, exited, new label, increased restarts
        store
            .write_snapshot(crate::storage::TelemetrySnapshot {
                timestamp: "2026-01-01T13:00:00Z".to_owned(),
                containers: vec![Container {
                    id: "abc".to_owned(),
                    name: "api".to_owned(),
                    image: "api:v2".to_owned(),
                    status: "Exited (1) 1 minute ago".to_owned(),
                    state: "exited".to_owned(),
                    ports: Vec::new(),
                    labels: vec!["env=prod".to_owned(), "team=infra".to_owned()],
                    created_at: None,
                    started_at: None,
                    finished_at: None,
                    restart_count: Some(3),
                    health: None,
                }],
                images: Vec::new(),
                networks: Vec::new(),
                volumes: Vec::new(),
            })
            .unwrap();

        let anomalies = detect_config_drift(&store).unwrap();
        let kinds: Vec<&str> = anomalies.iter().map(|a| a.kind.as_str()).collect();

        assert!(
            kinds.contains(&"config_drift"),
            "should detect image change"
        );
        assert!(
            kinds.contains(&"state_change"),
            "should detect state change"
        );
        assert!(
            kinds.contains(&"restart_increase"),
            "should detect restart increase"
        );
        assert!(kinds.contains(&"label_drift"), "should detect label change");
    }

    #[test]
    fn config_drift_returns_empty_when_no_changes() {
        let mut store = crate::storage::InMemoryTelemetryStore::default();

        // Two identical snapshots
        let snapshot = crate::storage::TelemetrySnapshot {
            timestamp: "2026-01-01T12:00:00Z".to_owned(),
            containers: vec![Container {
                id: "abc".to_owned(),
                name: "api".to_owned(),
                image: "api:latest".to_owned(),
                status: "Up 5 minutes".to_owned(),
                state: "running".to_owned(),
                ports: Vec::new(),
                labels: vec!["env=prod".to_owned()],
                created_at: None,
                started_at: None,
                finished_at: None,
                restart_count: Some(0),
                health: None,
            }],
            images: Vec::new(),
            networks: Vec::new(),
            volumes: Vec::new(),
        };
        store.write_snapshot(snapshot.clone()).unwrap();
        store.write_snapshot(snapshot).unwrap();

        let anomalies = detect_config_drift(&store).unwrap();
        assert!(anomalies.is_empty(), "should be empty when no changes");
    }

    #[test]
    fn config_drift_with_empty_store_returns_unsupported() {
        let store = crate::storage::InMemoryTelemetryStore::default();
        let result = detect_config_drift(&store);
        assert!(result.is_err());
    }

    // ── Density tests ───────────────────────────────────────────────

    #[test]
    fn analyzes_density_by_image_state_and_project() {
        let docker = crate::docker::MockDockerClient {
            containers: vec![
                Container {
                    image: "nginx:latest".to_owned(),
                    labels: vec!["com.docker.compose.project=web".to_owned()],
                    ..test_container("web-1", "running", 0)
                },
                Container {
                    image: "nginx:latest".to_owned(),
                    labels: vec!["com.docker.compose.project=web".to_owned()],
                    ..test_container("web-2", "running", 0)
                },
                Container {
                    image: "api:latest".to_owned(),
                    labels: vec![],
                    ..test_container("api", "exited", 0)
                },
            ],
            ..Default::default()
        };

        let result = rt().block_on(analyze_density(&docker)).unwrap();

        // Should have image rows (nginx, api) + state rows (running, exited) + project rows (web, standalone) + total
        let dimensions: Vec<String> = result
            .rows
            .iter()
            .filter_map(|r| {
                r.fields
                    .get("dimension")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            })
            .collect();
        assert!(dimensions.contains(&"image".to_owned()));
        assert!(dimensions.contains(&"state".to_owned()));
        assert!(dimensions.contains(&"compose_project".to_owned()));
        assert!(dimensions.contains(&"total".to_owned()));

        // Verify total
        let total_row = result
            .rows
            .iter()
            .find(|r| r.fields.get("dimension") == Some(&JsonValue::String("total".to_owned())))
            .unwrap();
        assert_eq!(
            total_row.fields["container_count"],
            JsonValue::Number(Number::from(3))
        );
    }

    #[test]
    fn density_with_no_containers_returns_total_zero() {
        let docker = crate::docker::MockDockerClient::default();
        let result = rt().block_on(analyze_density(&docker)).unwrap();
        let total_row = result
            .rows
            .iter()
            .find(|r| r.fields.get("dimension") == Some(&JsonValue::String("total".to_owned())))
            .unwrap();
        assert_eq!(
            total_row.fields["container_count"],
            JsonValue::Number(Number::from(0))
        );
    }

    // ── Integration: new subjects from dispatcher ────────────────────

    #[test]
    fn execute_analyze_find_dependencies_from_live_data() {
        let docker = crate::docker::MockDockerClient {
            containers: vec![test_container("api", "running", 0)],
            ..Default::default()
        };
        let metrics = crate::metrics::MockMetricsCollector {
            samples: Vec::new(),
        };

        let query = AnalyzeQuery {
            target: AnalysisTarget::Collection(CollectionTarget::Containers),
            verb: AnalysisVerb::Find,
            subject: Some("dependencies".to_owned()),
            time: None,
            pipeline: Vec::new(),
        };

        let result = rt().block_on(execute_analyze(&query, &docker, &metrics)).unwrap();
        // Should have at least the "none" row
        assert!(!result.rows.is_empty());
    }

    #[test]
    fn execute_analyze_find_density_from_live_data() {
        let docker = crate::docker::MockDockerClient {
            containers: vec![test_container("api", "running", 0)],
            ..Default::default()
        };
        let metrics = crate::metrics::MockMetricsCollector {
            samples: Vec::new(),
        };

        let query = AnalyzeQuery {
            target: AnalysisTarget::Collection(CollectionTarget::Containers),
            verb: AnalysisVerb::Find,
            subject: Some("density".to_owned()),
            time: None,
            pipeline: Vec::new(),
        };

        let result = rt().block_on(execute_analyze(&query, &docker, &metrics)).unwrap();
        let total_row = result
            .rows
            .iter()
            .find(|r| r.fields.get("dimension") == Some(&JsonValue::String("total".to_owned())))
            .unwrap();
        assert_eq!(
            total_row.fields["container_count"],
            JsonValue::Number(Number::from(1))
        );
    }

    #[test]
    fn execute_analyze_find_leaks_returns_unsupported_from_live() {
        let docker = crate::docker::MockDockerClient::default();
        let metrics = crate::metrics::MockMetricsCollector {
            samples: Vec::new(),
        };

        let query = AnalyzeQuery {
            target: AnalysisTarget::Collection(CollectionTarget::Containers),
            verb: AnalysisVerb::Find,
            subject: Some("leaks".to_owned()),
            time: None,
            pipeline: Vec::new(),
        };

        let result = rt().block_on(execute_analyze(&query, &docker, &metrics));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AnalyzeError::Unsupported));
    }

    #[test]
    fn execute_analyze_with_store_find_leaks_detects_pattern() {
        let mut store = crate::storage::InMemoryTelemetryStore::default();
        for i in 0..5 {
            let mem = 100_000_000 + (i * 30_000_000);
            store
                .write_metric(MetricSample {
                    container_id: "leaky_id".to_string(),
                    container_name: "leaky".to_owned(),
                    timestamp: format!("2026-01-01T12:00:{:02}Z", i),
                    cpu_percent: Some(50.0),
                    memory_usage_bytes: Some(mem),
                    memory_limit_bytes: Some(1_073_741_824),
                    network_rx_bytes: Some(0),
                    network_tx_bytes: Some(0),
                    disk_read_bytes: Some(0),
                    disk_write_bytes: Some(0),
                })
                .unwrap();
        }

        let query = AnalyzeQuery {
            target: AnalysisTarget::Collection(CollectionTarget::Containers),
            verb: AnalysisVerb::Find,
            subject: Some("leaks".to_owned()),
            time: None,
            pipeline: Vec::new(),
        };

        let result = execute_analyze_with_store(&query, &store).unwrap();
        let kinds: Vec<String> = result
            .rows
            .iter()
            .filter_map(|r| {
                r.fields
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            })
            .collect();
        assert!(kinds.contains(&"resource_leak".to_owned()));
    }

    #[test]
    fn execute_analyze_with_store_find_drift_from_snapshot() {
        let mut store = crate::storage::InMemoryTelemetryStore::default();
        store
            .write_snapshot(crate::storage::TelemetrySnapshot {
                timestamp: "2026-01-01T12:00:00Z".to_owned(),
                containers: vec![Container {
                    id: "abc".to_owned(),
                    name: "api".to_owned(),
                    image: "api:latest".to_owned(),
                    status: "Up 5m".to_owned(),
                    state: "running".to_owned(),
                    ports: Vec::new(),
                    labels: vec!["env=prod".to_owned()],
                    created_at: None,
                    started_at: None,
                    finished_at: None,
                    restart_count: Some(0),
                    health: None,
                }],
                images: Vec::new(),
                networks: Vec::new(),
                volumes: Vec::new(),
            })
            .unwrap();

        let query = AnalyzeQuery {
            target: AnalysisTarget::Collection(CollectionTarget::Containers),
            verb: AnalysisVerb::Find,
            subject: Some("drift".to_owned()),
            time: None,
            pipeline: Vec::new(),
        };

        let result = execute_analyze_with_store(&query, &store).unwrap();
        let kinds: Vec<String> = result
            .rows
            .iter()
            .filter_map(|r| {
                r.fields
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            })
            .collect();
        assert!(kinds.contains(&"config_baseline".to_owned()));
    }

    // ── Pre-existing tests ───────────────────────────────────────────

    #[test]
    fn find_anomalies_from_store_detects_deployment_errors() {
        let mut store = crate::storage::InMemoryTelemetryStore::default();

        store
            .write_event(test_event("2026-01-01T12:00:00Z", "die", "api"))
            .unwrap();
        store
            .write_event(test_event("2026-01-01T12:01:00Z", "die", "api"))
            .unwrap();
        store
            .write_event(test_event("2026-01-01T12:02:00Z", "die", "api"))
            .unwrap();

        let query = AnalyzeQuery {
            target: AnalysisTarget::Collection(CollectionTarget::Containers),
            verb: AnalysisVerb::Find,
            subject: Some("anomalies".to_owned()),
            time: None,
            pipeline: Vec::new(),
        };

        let result = execute_analyze_with_store(&query, &store).unwrap();

        let kinds: Vec<String> = result
            .rows
            .iter()
            .filter_map(|r| {
                r.fields
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            })
            .collect();
        assert!(kinds.contains(&"deployment_errors".to_owned()));
    }
}
