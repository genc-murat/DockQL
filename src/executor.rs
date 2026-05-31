use std::{
    collections::{BTreeMap, HashMap},
};

use serde::Serialize;
use serde_json::{Number, Value as JsonValue};
use thiserror::Error;

use crate::{
    analyze::{self, AnalyzeError},
    ast::{
        CollectionTarget, Expression, ObserveQuery, PipelineNode, Query, SortDirection,
    },
    docker::{Container, DockerClient, DockerError, Image, MetricSample, Network, Volume},
    eval::{self, EvalError},
    metrics::{MetricsCollector, MetricsError, NoopMetricsCollector},
    storage::{self, TelemetryError, TelemetryStore},
};

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct ExecutionResult {
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
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
        Query::Analyze(query) => analyze::execute_analyze_with_store(query, store)
            .map_err(ExecutorError::Analyze),
        Query::Inspect(_) => Err(ExecutorError::UnsupportedQuery("inspect")),
        Query::Events(_) => Err(ExecutorError::UnsupportedQuery("events")),
        Query::Observe(_) => Err(ExecutorError::UnsupportedQuery("observe historical")),
        Query::Alert(_) => Err(ExecutorError::UnsupportedQuery("alert")),
    }
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
        PipelineNode::SortBy { field, direction } => {
            sort_rows(&mut rows, field, *direction)?;
            Ok(rows)
        }
        PipelineNode::Limit(limit) => {
            rows.truncate(*limit as usize);
            Ok(rows)
        }
        PipelineNode::GroupBy(fields) => Ok(group_rows(rows, fields)),
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
        let value = row
            .fields
            .get(field)
            .ok_or_else(|| EvalError::UnsupportedField {
                field: field.clone(),
            })?;
        selected.insert(field.clone(), value.clone());
    }

    Ok(Row { fields: selected })
}

fn sort_rows(rows: &mut [Row], field: &str, direction: SortDirection) -> Result<(), ExecutorError> {
    if rows.iter().any(|row| !row.fields.contains_key(field)) {
        return Err(EvalError::UnsupportedField {
            field: field.to_owned(),
        }
        .into());
    }

    rows.sort_by(|left, right| {
        let ordering = eval::compare_json_values(&left.fields[field], &right.fields[field]);
        match direction {
            SortDirection::Asc => ordering,
            SortDirection::Desc => ordering.reverse(),
        }
    });
    Ok(())
}

fn container_row(container: Container, samples: &HashMap<String, MetricSample>) -> Row {
    let sample = samples
        .get(&container.id)
        .or_else(|| samples.get(&container.name));

    Row::from_fields([
        ("id", json_string(container.id)),
        ("name", json_string(container.name)),
        ("image", json_string(container.image)),
        ("status", json_string(container.status)),
        ("state", json_string(container.state)),
        ("ports", json_string_array(container.ports)),
        ("labels", json_string_array(container.labels)),
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
}
