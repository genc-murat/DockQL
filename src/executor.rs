use std::{
    collections::{BTreeMap, HashMap, HashSet},
};
use serde_json::{Number, Value as JsonValue};
use thiserror::Error;

use crate::{
    analyze::{self, AnalyzeError},
    ast::{
        CollectionTarget, DurationUnit, Expression, ObserveQuery, PipelineNode, Query,
        SortDirection,
    },
    docker::{Container, DockerClient, DockerError, Image, MetricSample, Network, Volume},
    eval::{self, EvalError},
    metrics::{MetricsCollector, MetricsError, NoopMetricsCollector},
    storage::{self, TelemetryError, TelemetryStore},
};

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize)]
pub struct ExecutionResult {
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize)]
pub struct Row {
    pub fields: BTreeMap<String, JsonValue>,
}

#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("{0}")]
    Docker(DockerError),
    #[error("{0}")]
    Metrics(MetricsError),
    #[error("{0}")]
    Telemetry(TelemetryError),
    #[error("{0}")]
    Analyze(AnalyzeError),
    #[error("unsupported query for batch executor: {0}")]
    UnsupportedQuery(&'static str),
    #[error("unsupported pipeline node for batch executor: {0}")]
    UnsupportedPipeline(&'static str),
    #[error("{0}")]
    Eval(#[from] EvalError),
    #[error("no store snapshot available for {0}")]
    SnapshotNotFound(&'static str),
}

impl From<DockerError> for ExecutorError {
    fn from(error: DockerError) -> Self {
        Self::Docker(error)
    }
}

impl From<MetricsError> for ExecutorError {
    fn from(error: MetricsError) -> Self {
        Self::Metrics(error)
    }
}

impl From<TelemetryError> for ExecutorError {
    fn from(error: TelemetryError) -> Self {
        Self::Telemetry(error)
    }
}

impl From<AnalyzeError> for ExecutorError {
    fn from(error: AnalyzeError) -> Self {
        Self::Analyze(error)
    }
}

pub fn execute<C>(query: &Query, docker: &C) -> Result<ExecutionResult, ExecutorError>
where
    C: DockerClient + ?Sized,
{
    execute_with_metrics(query, docker, &NoopMetricsCollector)
}

pub fn execute_with_metrics<C, M>(
    query: &Query,
    docker: &C,
    metrics: &M,
) -> Result<ExecutionResult, ExecutorError>
where
    C: DockerClient + ?Sized,
    M: MetricsCollector + ?Sized,
{
    match query {
        Query::Observe(query) => execute_observe(query, docker, metrics),
        Query::Events(_) => Err(ExecutorError::UnsupportedQuery("events")),
        Query::Inspect(_) => Err(ExecutorError::UnsupportedQuery("inspect")),
        Query::Analyze(query) => analyze::execute_analyze(query, docker, metrics)
            .map_err(ExecutorError::Analyze),
        Query::Alert(_) => Err(ExecutorError::UnsupportedQuery("alert")),
        Query::Fields(target) => execute_fields(*target),
    }
}

pub fn execute_with_store<S>(query: &Query, store: &S) -> Result<ExecutionResult, ExecutorError>
where
    S: TelemetryStore + ?Sized,
{
    match query {
        Query::Inspect(query) if query.at.is_some() => storage::inspect_at(query, store).map_err(Into::into),
        Query::Events(query) if query.time.is_some() => {
            storage::historical_events(query, store).map_err(Into::into)
        }
        Query::Observe(query) if query.time.is_some() => {
            historical_observe(query, store)
        }
        Query::Analyze(query) => analyze::execute_analyze_with_store(query, store)
            .map_err(ExecutorError::Analyze),
        Query::Inspect(_) => Err(ExecutorError::UnsupportedQuery("inspect")),
        Query::Events(_) => Err(ExecutorError::UnsupportedQuery("events")),
        Query::Observe(_) => Err(ExecutorError::UnsupportedQuery("observe historical")),
        Query::Alert(_) => Err(ExecutorError::UnsupportedQuery("alert")),
        Query::Fields(target) => execute_fields(*target),
    }
}

fn historical_observe<S>(query: &ObserveQuery, store: &S) -> Result<ExecutionResult, ExecutorError>
where
    S: TelemetryStore + ?Sized,
{
    use crate::ast::TimeSelector;

    let timestamp = match &query.time {
        Some(TimeSelector::Last(dur)) => {
            let secs = match dur.unit {
                DurationUnit::Seconds => dur.value as i64,
                DurationUnit::Minutes => dur.value as i64 * 60,
                DurationUnit::Hours => dur.value as i64 * 3600,
                DurationUnit::Days => dur.value as i64 * 86400,
            };
            let now = chrono::Utc::now();
            (now - chrono::Duration::seconds(secs)).to_rfc3339()
        }
        Some(TimeSelector::Range { from, to: _ }) => from.clone(),
        None => return Err(ExecutorError::UnsupportedQuery("observe historical")),
    };

    let snapshot = store.snapshot_at_or_before(&timestamp)
        .map_err(ExecutorError::Telemetry)?
        .ok_or_else(|| ExecutorError::SnapshotNotFound("historical_observe"))?;

    let mut rows: Vec<Row> = match query.target {
        CollectionTarget::Containers => {
            snapshot.containers.into_iter().map(|c| {
                let mut fields = BTreeMap::new();
                fields.insert("snapshot_at".into(), JsonValue::String(snapshot.timestamp.clone()));
                fields.insert("id".into(), json_string(c.id));
                fields.insert("name".into(), json_string(c.name));
                fields.insert("image".into(), json_string(c.image));
                fields.insert("status".into(), json_string(c.status));
                fields.insert("state".into(), json_string(c.state));
                fields.insert("restart_count".into(), c.restart_count.map(json_u64).unwrap_or(JsonValue::Null));
                Row { fields }
            }).collect()
        }
        CollectionTarget::Images => {
            snapshot.images.into_iter().map(|img| {
                let mut fields = BTreeMap::new();
                fields.insert("snapshot_at".into(), JsonValue::String(snapshot.timestamp.clone()));
                fields.insert("id".into(), json_string(img.id));
                fields.insert("repository".into(), json_string(img.repository));
                fields.insert("tag".into(), json_string(img.tag));
                fields.insert("size".into(), json_string(img.size));
                Row { fields }
            }).collect()
        }
        CollectionTarget::Networks => {
            snapshot.networks.into_iter().map(|n| {
                let mut fields = BTreeMap::new();
                fields.insert("snapshot_at".into(), JsonValue::String(snapshot.timestamp.clone()));
                fields.insert("id".into(), json_string(n.id));
                fields.insert("name".into(), json_string(n.name));
                fields.insert("driver".into(), json_string(n.driver));
                Row { fields }
            }).collect()
        }
        CollectionTarget::Volumes => {
            snapshot.volumes.into_iter().map(|v| {
                let mut fields = BTreeMap::new();
                fields.insert("snapshot_at".into(), JsonValue::String(snapshot.timestamp.clone()));
                fields.insert("name".into(), json_string(v.name));
                fields.insert("driver".into(), json_string(v.driver));
                Row { fields }
            }).collect()
        }
    };

    if let Some(filter) = &query.filter {
        rows = filter_rows(rows, filter)?;
    }

    for node in &query.pipeline {
        rows = apply_pipeline_node(rows, node)?;
    }

    Ok(ExecutionResult { rows })
}

pub fn render_table(result: &ExecutionResult) -> String {
    if result.rows.is_empty() {
        return "No rows".to_owned();
    }

    let columns = result.rows[0].fields.keys().cloned().collect::<Vec<_>>();
    let mut widths = columns
        .iter()
        .map(|column| column.len())
        .collect::<Vec<_>>();
    let rendered_rows = result
        .rows
        .iter()
        .map(|row| {
            columns
                .iter()
                .enumerate()
                .map(|(index, column)| {
                    let value = row
                        .fields
                        .get(column)
                        .map(eval::render_json_cell)
                        .unwrap_or_default();
                    widths[index] = widths[index].max(value.len());
                    value
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    let mut lines = Vec::new();
    lines.push(render_table_line(&columns, &widths));
    lines.push(
        widths
            .iter()
            .map(|width| "-".repeat(*width))
            .collect::<Vec<_>>()
            .join("  "),
    );
    lines.extend(
        rendered_rows
            .iter()
            .map(|row| render_table_line(row, &widths)),
    );
    lines.join("\n")
}

fn execute_fields(target: CollectionTarget) -> Result<ExecutionResult, ExecutorError> {
    let fields = field_descriptions(target);
    let rows = fields
        .into_iter()
        .map(|(field, kind)| {
            let mut map = BTreeMap::new();
            map.insert("field".to_owned(), JsonValue::String(field));
            map.insert("type".to_owned(), JsonValue::String(kind));
            Row { fields: map }
        })
        .collect();
    Ok(ExecutionResult { rows })
}

fn field_descriptions(target: CollectionTarget) -> Vec<(String, String)> {
    match target {
        CollectionTarget::Containers => vec![
            ("id".into(), "string".into()),
            ("name".into(), "string".into()),
            ("image".into(), "string".into()),
            ("status".into(), "string".into()),
            ("state".into(), "string".into()),
            ("ports".into(), "array".into()),
            ("labels".into(), "array".into()),
            ("compose_project".into(), "string".into()),
            ("created_at".into(), "string".into()),
            ("started_at".into(), "string".into()),
            ("finished_at".into(), "string".into()),
            ("restart_count".into(), "integer".into()),
            ("cpu".into(), "float".into()),
            ("memory".into(), "integer".into()),
            ("memory_limit".into(), "integer".into()),
            ("network_rx".into(), "integer".into()),
            ("network_tx".into(), "integer".into()),
            ("disk_read".into(), "integer".into()),
            ("disk_write".into(), "integer".into()),
        ],
        CollectionTarget::Images => vec![
            ("id".into(), "string".into()),
            ("repository".into(), "string".into()),
            ("name".into(), "string".into()),
            ("tag".into(), "string".into()),
            ("digest".into(), "string".into()),
            ("size".into(), "string".into()),
            ("created_at".into(), "string".into()),
            ("labels".into(), "array".into()),
        ],
        CollectionTarget::Networks => vec![
            ("id".into(), "string".into()),
            ("name".into(), "string".into()),
            ("driver".into(), "string".into()),
            ("scope".into(), "string".into()),
            ("containers".into(), "array".into()),
            ("labels".into(), "array".into()),
        ],
        CollectionTarget::Volumes => vec![
            ("name".into(), "string".into()),
            ("driver".into(), "string".into()),
            ("mountpoint".into(), "string".into()),
            ("scope".into(), "string".into()),
            ("labels".into(), "array".into()),
        ],
    }
}

fn execute_observe<C, M>(
    query: &ObserveQuery,
    docker: &C,
    metrics: &M,
) -> Result<ExecutionResult, ExecutorError>
where
    C: DockerClient + ?Sized,
    M: MetricsCollector + ?Sized,
{
    let mut rows = match query.target {
        CollectionTarget::Containers => {
            let samples = latest_metrics_by_container(metrics.collect()?);
            docker
                .list_containers()?
                .into_iter()
                .map(|container| container_row(container, &samples))
                .collect::<Vec<_>>()
        }
        CollectionTarget::Images => docker
            .list_images()?
            .into_iter()
            .map(image_row)
            .collect::<Vec<_>>(),
        CollectionTarget::Networks => docker
            .list_networks()?
            .into_iter()
            .map(network_row)
            .collect::<Vec<_>>(),
        CollectionTarget::Volumes => docker
            .list_volumes()?
            .into_iter()
            .map(volume_row)
            .collect::<Vec<_>>(),
    };

    if let Some(filter) = &query.filter {
        rows = filter_rows(rows, filter)?;
    }

    for node in &query.pipeline {
        rows = apply_pipeline_node(rows, node)?;
    }

    Ok(ExecutionResult { rows })
}

fn apply_pipeline_node(mut rows: Vec<Row>, node: &PipelineNode) -> Result<Vec<Row>, ExecutorError> {
    match node {
        PipelineNode::Where(expression) => filter_rows(rows, expression),
        PipelineNode::Select(fields) => rows
            .into_iter()
            .map(|row| select_fields(row, fields))
            .collect(),
        PipelineNode::SortBy { fields } => {
            sort_rows(&mut rows, fields)?;
            Ok(rows)
        }
        PipelineNode::Limit(limit) => {
            rows.truncate(*limit as usize);
            Ok(rows)
        }
        PipelineNode::GroupBy { fields, .. } => Ok(group_rows(rows, fields)),
        PipelineNode::Having(expr) => filter_rows(rows, expr),
        PipelineNode::Distinct => {
            let mut seen = HashSet::new();
            rows.retain(|row| {
                let key: Vec<String> = row.fields.values().map(eval::render_json_cell).collect();
                seen.insert(key)
            });
            Ok(rows)
        }
        PipelineNode::Offset(offset) => {
            let skip = *offset as usize;
            if skip < rows.len() {
                rows = rows.split_off(skip);
            } else {
                rows.clear();
            }
            Ok(rows)
        }
        PipelineNode::Alert(message) => {
            for row in &rows {
                let output = row
                    .fields
                    .iter()
                    .map(|(k, v)| format!("{k}={}", eval::render_json_cell(v)))
                    .collect::<Vec<_>>()
                    .join(", ");
                eprintln!("[ALERT] {message}: {output}");
            }
            Ok(rows)
        }
        PipelineNode::If {
            condition,
            then_branch,
            else_branch,
        } => {
            let empty = Vec::new();
            let mut result = Vec::new();
            for row in rows {
                let matched = eval::evaluate_expression(&row.fields, condition)?;
                let branch = if matched {
                    then_branch
                } else {
                    else_branch.as_ref().unwrap_or(&empty)
                };
                let mut current = vec![row];
                for node in branch {
                    current = apply_pipeline_node(current, node)?;
                }
                result.extend(current);
            }
            Ok(result)
        }
        PipelineNode::Set { field, value } => {
            for row in &mut rows {
                let json_value = eval::evaluate_set_value(&row.fields, value)?;
                row.fields.insert(field.clone(), json_value);
            }
            Ok(rows)
        }
    }
}

fn filter_rows(rows: Vec<Row>, expression: &Expression) -> Result<Vec<Row>, ExecutorError> {
    rows.into_iter()
        .map(|row| eval::evaluate_expression(&row.fields, expression).map(|keep| (row, keep)))
        .filter_map(|result| match result {
            Ok((row, true)) => Some(Ok(row)),
            Ok((_, false)) => None,
            Err(error) => Some(Err(error.into())),
        })
        .collect()
}

fn select_fields(row: Row, fields: &[String]) -> Result<Row, ExecutorError> {
    let mut selected = BTreeMap::new();

    for field in fields {
        if let Some(value) = row.fields.get(field) {
            selected.insert(field.clone(), value.clone());
        } else if let Some(label_key) = field.strip_prefix("label.") {
            if let Some(JsonValue::Array(items)) = row.fields.get("labels") {
                for item in items {
                    if let JsonValue::String(entry) = item {
                        if let Some(eq_pos) = entry.find('=') {
                            let key = &entry[..eq_pos];
                            let val = &entry[eq_pos + 1..];
                            if key == label_key {
                                selected.insert(field.clone(), JsonValue::String(val.to_owned()));
                                break;
                            }
                        }
                    }
                }
            }
        } else {
            return Err(EvalError::UnsupportedField {
                field: field.clone(),
            }
            .into());
        }
    }

    Ok(Row { fields: selected })
}

fn sort_rows(rows: &mut [Row], fields: &[(String, SortDirection)]) -> Result<(), ExecutorError> {
    fn resolve_sort_value(row: &Row, field: &str) -> Option<JsonValue> {
        if let Some(v) = row.fields.get(field) {
            return Some(v.clone());
        }
        if let Some(label_key) = field.strip_prefix("label.") {
            if let Some(JsonValue::Array(items)) = row.fields.get("labels") {
                for item in items {
                    if let JsonValue::String(entry) = item {
                        if let Some(eq_pos) = entry.find('=') {
                            let key = &entry[..eq_pos];
                            let val = &entry[eq_pos + 1..];
                            if key == label_key {
                                return Some(JsonValue::String(val.to_owned()));
                            }
                        }
                    }
                }
            }
        }
        None
    }

    // Validate all sort fields exist
    for (field, _) in fields {
        if rows.iter().any(|row| resolve_sort_value(row, field).is_none()) {
            return Err(EvalError::UnsupportedField {
                field: field.to_owned(),
            }
            .into());
        }
    }

    rows.sort_by(|left, right| {
        for (field, direction) in fields {
            let left_val = resolve_sort_value(left, field).unwrap_or(JsonValue::Null);
            let right_val = resolve_sort_value(right, field).unwrap_or(JsonValue::Null);
            let ordering = eval::compare_json_values(&left_val, &right_val);
            let ordering = match direction {
                SortDirection::Asc => ordering,
                SortDirection::Desc => ordering.reverse(),
            };
            if ordering != std::cmp::Ordering::Equal {
                return ordering;
            }
        }
        std::cmp::Ordering::Equal
    });
    Ok(())
}

fn container_row(container: Container, samples: &HashMap<String, MetricSample>) -> Row {
    let sample = samples
        .get(&container.id)
        .or_else(|| samples.get(&container.name));

    let compose_project = container.labels.iter()
        .find_map(|label| {
            let parts: Vec<&str> = label.splitn(2, '=').collect();
            if parts.len() == 2 && parts[0] == "com.docker.compose.project" {
                Some(parts[1].to_owned())
            } else {
                None
            }
        });

    Row::from_fields([
        ("id", json_string(container.id)),
        ("name", json_string(container.name)),
        ("image", json_string(container.image)),
        ("status", json_string(container.status)),
        ("state", json_string(container.state)),
        ("ports", json_string_array(container.ports)),
        ("labels", json_string_array(container.labels)),
        ("compose_project", compose_project.map(JsonValue::String).unwrap_or(JsonValue::Null)),
        ("created_at", json_option_string(container.created_at)),
        ("started_at", json_option_string(container.started_at)),
        ("finished_at", json_option_string(container.finished_at)),
        (
            "restart_count",
            container
                .restart_count
                .map(json_u64)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "cpu",
            sample
                .and_then(|sample| sample.cpu_percent)
                .map(json_f64)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "memory",
            sample
                .and_then(|sample| sample.memory_usage_bytes)
                .map(json_u64)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "memory_limit",
            sample
                .and_then(|sample| sample.memory_limit_bytes)
                .map(json_u64)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "network_rx",
            sample
                .and_then(|sample| sample.network_rx_bytes)
                .map(json_u64)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "network_tx",
            sample
                .and_then(|sample| sample.network_tx_bytes)
                .map(json_u64)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "disk_read",
            sample
                .and_then(|sample| sample.disk_read_bytes)
                .map(json_u64)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "disk_write",
            sample
                .and_then(|sample| sample.disk_write_bytes)
                .map(json_u64)
                .unwrap_or(JsonValue::Null),
        ),
    ])
}

fn image_row(image: Image) -> Row {
    Row::from_fields([
        ("id", json_string(image.id)),
        ("repository", json_string(image.repository.clone())),
        ("name", json_string(image.repository)),
        ("tag", json_string(image.tag)),
        ("digest", json_option_string(image.digest)),
        ("size", json_string(image.size)),
        ("created_at", json_option_string(image.created_at)),
        ("labels", json_string_array(image.labels)),
    ])
}

fn network_row(network: Network) -> Row {
    Row::from_fields([
        ("id", json_string(network.id)),
        ("name", json_string(network.name)),
        ("driver", json_string(network.driver)),
        ("scope", json_string(network.scope)),
        ("containers", json_string_array(network.containers)),
        ("labels", json_string_array(network.labels)),
    ])
}

fn volume_row(volume: Volume) -> Row {
    Row::from_fields([
        ("name", json_string(volume.name)),
        ("driver", json_string(volume.driver)),
        ("mountpoint", json_option_string(volume.mountpoint)),
        ("scope", json_option_string(volume.scope)),
        ("labels", json_string_array(volume.labels)),
    ])
}

impl Row {
    fn from_fields<const N: usize>(fields: [(&str, JsonValue); N]) -> Self {
        Self {
            fields: fields
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        }
    }
}

fn render_table_line(values: &[String], widths: &[usize]) -> String {
    values
        .iter()
        .zip(widths)
        .map(|(value, width)| format!("{value:<width$}"))
        .collect::<Vec<_>>()
        .join("  ")
}

pub fn render_csv(result: &ExecutionResult) -> String {
    if result.rows.is_empty() {
        return String::new();
    }

    let columns: Vec<String> = result.rows[0].fields.keys().cloned().collect();

    let mut lines = vec![columns.join(",")];

    for row in &result.rows {
        let values: Vec<String> = columns
            .iter()
            .map(|col| {
                let val = row
                    .fields
                    .get(col)
                    .map(eval::render_json_cell)
                    .unwrap_or_default();
                if val.contains(',') || val.contains('"') || val.contains('\n') {
                    format!("\"{}\"", val.replace('"', "\"\""))
                } else {
                    val
                }
            })
            .collect();
        lines.push(values.join(","));
    }

    lines.join("\n")
}

pub fn render_jsonl(result: &ExecutionResult) -> String {
    result
        .rows
        .iter()
        .filter_map(|row| serde_json::to_string(&row).ok())
        .collect::<Vec<_>>()
        .join("\n")
}

fn ansi_color(value: &str) -> &'static str {
    match value {
        "running" => "\x1b[32m",
        "exited" | "dead" => "\x1b[31m",
        "restarting" | "paused" => "\x1b[33m",
        "created" => "\x1b[36m",
        "critical" => "\x1b[31;1m",
        "warning" => "\x1b[33m",
        "ok" | "healthy" => "\x1b[32m",
        _ => "\x1b[0m",
    }
}

const ANSI_RESET: &str = "\x1b[0m";

pub fn render_table_colored(result: &ExecutionResult) -> String {
    if result.rows.is_empty() {
        return "No rows".to_owned();
    }

    let columns = result.rows[0].fields.keys().cloned().collect::<Vec<_>>();
    let mut widths = columns
        .iter()
        .map(|column| column.len())
        .collect::<Vec<_>>();
    let rendered_rows = result
        .rows
        .iter()
        .map(|row| {
            columns
                .iter()
                .enumerate()
                .map(|(index, column)| {
                    let value = row
                        .fields
                        .get(column)
                        .map(eval::render_json_cell)
                        .unwrap_or_default();
                    widths[index] = widths[index].max(value.len());
                    value
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    let mut lines = Vec::new();
    lines.push(render_table_line(&columns, &widths));

    let header_color = "\x1b[1;37m";
    if let Some(first) = lines.last_mut() {
        *first = format!("{header_color}{first}{ANSI_RESET}");
    }

    lines.push(
        widths
            .iter()
            .map(|width| "-".repeat(*width))
            .collect::<Vec<_>>()
            .join("  "),
    );

    for row in &rendered_rows {
        let color = row.iter().find_map(|v| {
            let c = ansi_color(v);
            if c != "\x1b[0m" { Some(c) } else { None }
        }).unwrap_or("\x1b[0m");

        let line = render_table_line(row, &widths);
        lines.push(format!("{color}{line}{ANSI_RESET}"));
    }

    lines.join("\n")
}

pub fn render_diff<S>(current: &ExecutionResult, store: &S) -> Result<String, ExecutorError>
where
    S: crate::storage::TelemetryStore + ?Sized,
{
    let now = chrono::Utc::now().to_rfc3339();
    let snapshot = store.snapshot_at_or_before(&now)
        .map_err(ExecutorError::Telemetry)?
        .ok_or_else(|| ExecutorError::SnapshotNotFound("diff"))?;

    let prev_ids: HashSet<&str> = snapshot.containers.iter().map(|c| c.id.as_str()).collect();
    let curr_ids: HashSet<&str> = current.rows.iter()
        .filter_map(|r| r.fields.get("id").and_then(|v| v.as_str()))
        .collect();

    let added: Vec<&str> = curr_ids.difference(&prev_ids).copied().collect();
    let removed: Vec<&str> = prev_ids.difference(&curr_ids).copied().collect();
    let changed: Vec<&str> = curr_ids.intersection(&prev_ids).copied().collect();

    let mut lines = Vec::new();

    if !added.is_empty() {
        lines.push(format!("\x1b[32mAdded containers ({}):\x1b[0m", added.len()));
        for id in &added {
            if let Some(row) = current.rows.iter().find(|r| r.fields.get("id").and_then(|v| v.as_str()) == Some(id)) {
                let name = row.fields.get("name").map(eval::render_json_cell).unwrap_or_default();
                lines.push(format!("  \x1b[32m+ {name} ({id})\x1b[0m"));
            }
        }
    }

    if !removed.is_empty() {
        lines.push(format!("\x1b[31mRemoved containers ({}):\x1b[0m", removed.len()));
        for id in &removed {
            if let Some(c) = snapshot.containers.iter().find(|c| c.id == *id) {
                lines.push(format!("  \x1b[31m- {name} ({id})\x1b[0m", name = c.name));
            }
        }
    }

    if !changed.is_empty() {
        lines.push(format!("Changed containers ({}):", changed.len()));
        for id in &changed {
            if let Some(row) = current.rows.iter().find(|r| r.fields.get("id").and_then(|v| v.as_str()) == Some(id)) {
                let name = row.fields.get("name").map(eval::render_json_cell).unwrap_or_default();
                let state = row.fields.get("state").map(eval::render_json_cell).unwrap_or_default();
                if let Some(prev_c) = snapshot.containers.iter().find(|c| c.id == *id) {
                    if prev_c.state != state {
                        lines.push(format!("  ~ {name}: {prev} -> {state}", prev = prev_c.state));
                    }
                }
            }
        }
    }

    if added.is_empty() && removed.is_empty() {
        lines.push("No changes detected.".to_owned());
    }

    Ok(lines.join("\n"))
}

fn json_string(value: String) -> JsonValue {
    JsonValue::String(value)
}

fn json_option_string(value: Option<String>) -> JsonValue {
    value.map(JsonValue::String).unwrap_or(JsonValue::Null)
}

fn json_string_array(values: Vec<String>) -> JsonValue {
    JsonValue::Array(values.into_iter().map(JsonValue::String).collect())
}

fn json_u64(value: u64) -> JsonValue {
    JsonValue::Number(Number::from(value))
}

fn json_f64(value: f64) -> JsonValue {
    Number::from_f64(value)
        .map(JsonValue::Number)
        .unwrap_or(JsonValue::Null)
}

fn latest_metrics_by_container(samples: Vec<MetricSample>) -> HashMap<String, MetricSample> {
    let mut latest = HashMap::new();
    for sample in samples {
        latest.insert(sample.container_id.clone(), sample.clone());
        latest.insert(sample.container_name.clone(), sample);
    }
    latest
}

fn group_rows(rows: Vec<Row>, group_fields: &[String]) -> Vec<Row> {
    let mut groups: BTreeMap<Vec<String>, (Row, u64)> = BTreeMap::new();

    for row in rows {
        let key: Vec<String> = group_fields
            .iter()
            .map(|f| {
                row.fields
                    .get(f)
                    .map(eval::render_json_cell)
                    .unwrap_or_default()
            })
            .collect();

        let entry = groups.entry(key).or_insert_with(|| {
            let mut fields = BTreeMap::new();
            for f in group_fields {
                if let Some(v) = row.fields.get(f) {
                    fields.insert(f.clone(), v.clone());
                }
            }
            (Row { fields }, 0)
        });
        entry.1 += 1;
    }

    groups
        .into_iter()
        .map(|(_, (mut row, count))| {
            row.fields
                .insert("count".to_owned(), JsonValue::Number(Number::from(count)));
            row
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::{docker::MockDockerClient, metrics::MockMetricsCollector, parser};

    use super::*;

    #[test]
    fn observes_containers_from_mock_client() {
        let client = mock_client();
        let parsed = parser::parse("observe containers").expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 2);
        assert_eq!(
            result.rows[0].fields["name"],
            JsonValue::String("api".to_owned())
        );
    }

    #[test]
    fn filters_containers() {
        let client = mock_client();
        let parsed =
            parser::parse("observe containers where state = running").expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["name"],
            JsonValue::String("api".to_owned())
        );
    }

    #[test]
    fn applies_pipeline_select_sort_and_limit() {
        let client = mock_client();
        let parsed = parser::parse(
            "observe containers | sort by restart_count desc | limit 1 | select name, restart_count",
        )
        .expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields.keys().cloned().collect::<Vec<_>>(),
            vec!["name".to_owned(), "restart_count".to_owned()]
        );
        assert_eq!(
            result.rows[0].fields["name"],
            JsonValue::String("worker".to_owned())
        );
    }

    #[test]
    fn filters_containers_by_cpu_metric() {
        let client = mock_client();
        let metrics = MockMetricsCollector {
            samples: vec![
                MetricSample {
                    container_id: "abc".to_owned(),
                    container_name: "api".to_owned(),
                    timestamp: "2026-05-31T02:00:00Z".to_owned(),
                    cpu_percent: Some(87.5),
                    memory_usage_bytes: Some(128),
                    memory_limit_bytes: Some(1024),
                    network_rx_bytes: Some(10),
                    network_tx_bytes: Some(20),
                    disk_read_bytes: Some(30),
                    disk_write_bytes: Some(40),
                },
                MetricSample {
                    container_id: "def".to_owned(),
                    container_name: "worker".to_owned(),
                    timestamp: "2026-05-31T02:00:00Z".to_owned(),
                    cpu_percent: Some(12.0),
                    memory_usage_bytes: Some(64),
                    memory_limit_bytes: Some(1024),
                    network_rx_bytes: Some(1),
                    network_tx_bytes: Some(2),
                    disk_read_bytes: Some(3),
                    disk_write_bytes: Some(4),
                },
            ],
        };
        let parsed = parser::parse("observe containers | where cpu > 80% | select name, cpu")
            .expect("query should parse");

        let result =
            execute_with_metrics(&parsed.query, &client, &metrics).expect("query should execute");

        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["name"],
            JsonValue::String("api".to_owned())
        );
        assert_eq!(result.rows[0].fields["cpu"], json_f64(87.5));
    }

    #[test]
    fn observes_images() {
        let client = mock_client();
        let parsed =
            parser::parse("observe images | select repository, tag").expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["repository"],
            JsonValue::String("postgres".to_owned())
        );
    }

    #[test]
    fn groups_rows_by_state() {
        let client = mock_client();
        let parsed =
            parser::parse("observe containers | group by state").expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 2);
        let running = result
            .rows
            .iter()
            .find(|row| row.fields["state"] == JsonValue::String("running".to_owned()))
            .expect("running group");
        let exited = result
            .rows
            .iter()
            .find(|row| row.fields["state"] == JsonValue::String("exited".to_owned()))
            .expect("exited group");
        assert_eq!(running.fields["count"], JsonValue::Number(Number::from(1)));
        assert_eq!(exited.fields["count"], JsonValue::Number(Number::from(1)));
    }

    #[test]
    fn rejects_unknown_field() {
        let client = mock_client();
        let parsed =
            parser::parse("observe containers | select missing").expect("query should parse");

        let error = execute(&parsed.query, &client).unwrap_err();

        assert!(matches!(error, ExecutorError::Eval(eval::EvalError::UnsupportedField { .. })));
    }

    #[test]
    fn renders_table() {
        let result = ExecutionResult {
            rows: vec![Row::from_fields([
                ("name", JsonValue::String("api".to_owned())),
                ("state", JsonValue::String("running".to_owned())),
            ])],
        };

        let table = render_table(&result);

        assert!(table.contains("name"));
        assert!(table.contains("running"));
    }

    #[test]
    fn sets_literal_field() {
        let client = mock_client();
        let parsed = parser::parse("observe containers | set tier = \"prod\" | select name, tier")
            .expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[0].fields["tier"], JsonValue::String("prod".to_owned()));
    }

    #[test]
    fn sets_case_field() {
        let client = mock_client();
        let parsed = parser::parse(
            "observe containers | set health = case when state = running then \"up\" else \"down\" end | select name, health",
        ).expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[0].fields["health"], JsonValue::String("up".to_owned()));
        assert_eq!(result.rows[1].fields["health"], JsonValue::String("down".to_owned()));
    }

    #[test]
    fn applies_if_pipeline() {
        let client = mock_client();
        let parsed = parser::parse(
            "observe containers | if state = running then set status_label = \"active\" else set status_label = \"inactive\" | select name, status_label",
        ).expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[0].fields["status_label"], JsonValue::String("active".to_owned()));
        assert_eq!(result.rows[1].fields["status_label"], JsonValue::String("inactive".to_owned()));
    }

    fn mock_client() -> MockDockerClient {
        MockDockerClient {
            containers: vec![
                Container {
                    id: "abc".to_owned(),
                    name: "api".to_owned(),
                    image: "api:latest".to_owned(),
                    status: "Up 2 minutes".to_owned(),
                    state: "running".to_owned(),
                    ports: vec!["8080/tcp".to_owned()],
                    labels: vec!["role=api".to_owned()],
                    created_at: None,
                    started_at: None,
                    finished_at: None,
                    restart_count: Some(0),
                },
                Container {
                    id: "def".to_owned(),
                    name: "worker".to_owned(),
                    image: "worker:latest".to_owned(),
                    status: "Exited (1) 1 minute ago".to_owned(),
                    state: "exited".to_owned(),
                    ports: Vec::new(),
                    labels: vec!["role=worker".to_owned()],
                    created_at: None,
                    started_at: None,
                    finished_at: None,
                    restart_count: Some(4),
                },
            ],
            images: vec![Image {
                id: "img".to_owned(),
                repository: "postgres".to_owned(),
                tag: "16".to_owned(),
                digest: None,
                size: "432MB".to_owned(),
                created_at: None,
                labels: Vec::new(),
            }],
            networks: vec![Network {
                id: "net".to_owned(),
                name: "bridge".to_owned(),
                driver: "bridge".to_owned(),
                scope: "local".to_owned(),
                containers: vec!["api".to_owned()],
                labels: Vec::new(),
            }],
            volumes: vec![Volume {
                name: "pgdata".to_owned(),
                driver: "local".to_owned(),
                mountpoint: Some("/var/lib/docker/volumes/pgdata/_data".to_owned()),
                scope: Some("local".to_owned()),
                labels: Vec::new(),
            }],
        }
    }

    #[test]
    fn renders_csv() {
        let result = ExecutionResult {
            rows: vec![Row::from_fields([
                ("name", JsonValue::String("api".to_owned())),
                ("state", JsonValue::String("running".to_owned())),
            ])],
        };
        let csv = render_csv(&result);
        assert!(csv.contains("name,state"));
        assert!(csv.contains("api,running"));
    }

    #[test]
    fn renders_jsonl() {
        let result = ExecutionResult {
            rows: vec![Row::from_fields([
                ("name", JsonValue::String("api".to_owned())),
            ])],
        };
        let jsonl = render_jsonl(&result);
        assert!(jsonl.contains("\"name\":\"api\""));
    }

    #[test]
    fn label_field_access_in_pipeline() {
        let client = mock_client();
        let parsed = parser::parse(
            "observe containers | where label.role = \"api\" | select name, label.role",
        ).expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].fields["label.role"], JsonValue::String("api".to_owned()));
    }

    #[test]
    fn execute_fields_containers() {
        let result = execute_fields(CollectionTarget::Containers).expect("fields should execute");
        assert!(!result.rows.is_empty());
        let field_names: Vec<&str> = result.rows.iter()
            .map(|r| r.fields["field"].as_str().unwrap())
            .collect();
        assert!(field_names.contains(&"name"));
        assert!(field_names.contains(&"cpu"));
    }
}
