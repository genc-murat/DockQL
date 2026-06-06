//! Docker event streaming and collection.
//!
//! Provides [`EventSource`] trait, [`BollardEventSource`] (live Docker
//! events), and [`MockEventSource`] (testing). The [`stream_events`]
//! function processes events through a DOL pipeline in real-time, while
//! [`collect_events`] returns a batch result.
//!
//! # Example
//!
//! ```ignore
//! let source = BollardEventSource::new(Arc::new(docker));
//! events::stream_events(query, &source, |row| { println!("{row:?}"); Ok(()) }).await?;
//! ```

use std::{collections::BTreeMap, pin::Pin, sync::Arc};

use futures_util::{Stream, StreamExt};
use serde_json::{Number, Value as JsonValue};
use thiserror::Error;

use crate::{
    ast::{CollectionTarget, ComposeQuery, EventsQuery, LogsQuery, PipelineNode},
    docker::{DockerClient, DockerError, DockerEvent},
    eval::{self, EvalError},
    executor::{ExecutionResult, Row},
    json_string,
};

/// Shared type alias for the complex event stream return type.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<DockerEvent, EventsError>> + Send>>;

/// Shared type alias for the log stream return type (mapped to EventsError).
pub type LogStream = Pin<Box<dyn Stream<Item = Result<String, EventsError>> + Send>>;

/// Shared type alias for the row-based stream return type (mapped to EventsError).
pub type RowStream = Pin<Box<dyn Stream<Item = Result<Row, EventsError>> + Send>>;

pub trait EventSource {
    fn events_stream(
        &self,
    ) -> impl std::future::Future<Output = Result<EventStream, EventsError>> + Send
    where
        Self: Sync;
}

#[derive(Debug, Error)]
pub enum EventsError {
    #[error("{0}")]
    Docker(#[from] DockerError),
    #[error("failed to parse docker event JSON: {0}")]
    Json(serde_json::Error),
    #[error("unsupported event target: {0:?}")]
    UnsupportedTarget(CollectionTarget),
    #[error("unsupported event pipeline node: {0}")]
    UnsupportedPipeline(&'static str),
    #[error("{0}")]
    Eval(#[from] EvalError),
    #[error("assertion failed: {0}")]
    AssertionFailed(String),
}

// ── BollardEventSource ──────────────────────────────────────────────────────

/// An event source that uses a `DockerClient` (bollard) to stream Docker events.
pub struct BollardEventSource<C> {
    docker: std::sync::Arc<C>,
}

impl<C> BollardEventSource<C>
where
    C: DockerClient + Send + Sync + 'static,
{
    pub const fn new(docker: std::sync::Arc<C>) -> Self {
        Self { docker }
    }
}

impl<C> EventSource for BollardEventSource<C>
where
    C: DockerClient + Send + Sync + 'static,
{
    async fn events_stream(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<DockerEvent, EventsError>> + Send>>, EventsError>
    {
        let stream = self.docker.events_stream(None, None).await?;
        let mapped = stream.map(|result| result.map_err(EventsError::Docker));
        Ok(Box::pin(mapped))
    }
}

// ── LogSource trait + implementations ──────────────────────────────────────

pub trait LogSource {
    fn logs_stream(
        &self,
        container_id: &str,
        tail: usize,
    ) -> impl std::future::Future<Output = Result<LogStream, EventsError>> + Send
    where
        Self: Sync;
}

/// A log source that uses a `DockerClient` (bollard) to stream Docker container logs.
pub struct BollardLogSource<C> {
    docker: std::sync::Arc<C>,
}

impl<C> BollardLogSource<C>
where
    C: DockerClient + Send + Sync + 'static,
{
    pub const fn new(docker: std::sync::Arc<C>) -> Self {
        Self { docker }
    }
}

impl<C> LogSource for BollardLogSource<C>
where
    C: DockerClient + Send + Sync + 'static,
{
    async fn logs_stream(&self, container_id: &str, tail: usize) -> Result<LogStream, EventsError> {
        let stream = self
            .docker
            .container_logs_stream(container_id, tail)
            .await?;
        let mapped = stream.map(|result| result.map_err(EventsError::Docker));
        Ok(Box::pin(mapped))
    }
}

// ── MockLogSource ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct MockLogSource {
    pub lines: Vec<String>,
}

impl LogSource for MockLogSource {
    async fn logs_stream(
        &self,
        _container_id: &str,
        _tail: usize,
    ) -> Result<LogStream, EventsError> {
        let items: Vec<Result<String, EventsError>> =
            self.lines.clone().into_iter().map(Ok).collect();
        Ok(Box::pin(futures_util::stream::iter(items)))
    }
}

// ── MockEventSource ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct MockEventSource {
    pub events: Vec<DockerEvent>,
}

impl EventSource for MockEventSource {
    async fn events_stream(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<DockerEvent, EventsError>> + Send>>, EventsError>
    {
        let items: Vec<Result<DockerEvent, EventsError>> =
            self.events.clone().into_iter().map(Ok).collect();
        Ok(Box::pin(futures_util::stream::iter(items)))
    }
}

/// Convert a log message line into a [`Row`] with `line`, `message`, and `container` fields.
#[must_use]
pub fn log_row(container: &str, line_num: u64, message: String) -> Row {
    Row {
        fields: BTreeMap::from([
            ("line".to_owned(), JsonValue::Number(Number::from(line_num))),
            ("message".to_owned(), JsonValue::String(message)),
            ("container".to_owned(), json_string(container.to_owned())),
        ]),
    }
}

/// Collect log lines into an [`ExecutionResult`] (batch mode).
pub async fn collect_logs<S>(query: &LogsQuery, source: &S) -> Result<ExecutionResult, EventsError>
where
    S: LogSource + ?Sized + Sync,
{
    let tail = query.tail.unwrap_or(100) as usize;
    let limit = query.pipeline.iter().find_map(|node| match node {
        PipelineNode::Limit(limit) => Some(*limit),
        _ => None,
    });

    let mut rows = Vec::new();
    let mut line_num = 0u64;
    let mut stream = source.logs_stream(&query.container, tail).await?;
    while let Some(line) = stream.next().await {
        line_num += 1;
        let mut row = Some(log_row(&query.container, line_num, line?));

        if let Some(filter) = &query.filter
            && !eval::evaluate_expression(&row.as_ref().expect("row is present").fields, filter)?
        {
            continue;
        }

        let mut keep = true;
        for node in &query.pipeline {
            let current = row.take().expect("row is present while pipeline runs");
            match apply_pipeline_node(current, node)? {
                PipelineOutcome::Row(next_row) => row = Some(next_row),
                PipelineOutcome::Drop => {
                    keep = false;
                    break;
                }
                PipelineOutcome::LimitReached => {
                    return Ok(ExecutionResult { rows });
                }
            }
        }

        if keep {
            rows.push(row.expect("row is present after pipeline"));
            if limit.is_some_and(|limit| rows.len() as u64 >= limit) {
                break;
            }
        }
    }

    Ok(ExecutionResult { rows })
}

/// Stream compose service logs in real-time. Finds containers matching the compose
/// project + service, merges their log streams via [`futures_util::stream::select_all`],
/// and applies the compose query's pipeline to each row.
pub async fn stream_compose_logs<C, F>(
    query: &ComposeQuery,
    docker: Arc<C>,
    mut on_row: F,
) -> Result<(), EventsError>
where
    C: DockerClient + Send + Sync + 'static,
    F: FnMut(Row) -> Result<(), EventsError>,
{
    let service_name = query.service.as_deref().unwrap_or("");
    let tail = query.tail.unwrap_or(100) as usize;

    let containers = docker.list_containers().await?;
    let matching: Vec<_> = containers
        .into_iter()
        .filter(|c| {
            let has_project = c.labels.iter().any(|label| {
                let parts: Vec<&str> = label.splitn(2, '=').collect();
                parts.len() == 2
                    && parts[0] == "com.docker.compose.project"
                    && parts[1] == query.project
            });
            let has_service = c.labels.iter().any(|label| {
                let parts: Vec<&str> = label.splitn(2, '=').collect();
                parts.len() == 2
                    && parts[0] == "com.docker.compose.service"
                    && parts[1] == service_name
            });
            has_project && (service_name.is_empty() || has_service)
        })
        .collect();

    if matching.is_empty() {
        return Ok(());
    }

    // Create a log stream for each matching container, tagged with container + service
    let mut streams: Vec<RowStream> = Vec::new();
    for container in &matching {
        let raw = docker.container_logs_stream(&container.id, tail).await?;
        let container_name = container.name.clone();
        let svc = service_name.to_owned();
        let mapped = raw.map(move |line_result| match line_result {
            Ok(line) => Ok(Row {
                fields: BTreeMap::from([
                    ("message".to_owned(), JsonValue::String(line)),
                    ("container".to_owned(), json_string(container_name.clone())),
                    ("service".to_owned(), json_string(svc.clone())),
                ]),
            }),
            Err(e) => Err(EventsError::Docker(e)),
        });
        streams.push(Box::pin(mapped));
    }

    // Merge all container streams
    let mut merged = futures_util::stream::select_all(streams);

    let limit = query.pipeline.iter().find_map(|node| match node {
        PipelineNode::Limit(limit) => Some(*limit),
        _ => None,
    });
    let mut emitted = 0u64;
    let mut line_num = 0u64;

    while let Some(item) = merged.next().await {
        let mut row = Some(item?);
        line_num += 1;
        // Add line number to the row
        if let Some(ref mut r) = row {
            r.fields
                .insert("line".to_owned(), JsonValue::Number(Number::from(line_num)));
        }

        // Apply the compose query's pipeline
        let mut keep = true;
        for node in &query.pipeline {
            let current = row.take().expect("row is present while pipeline runs");
            match apply_pipeline_node(current, node)? {
                PipelineOutcome::Row(next_row) => row = Some(next_row),
                PipelineOutcome::Drop => {
                    keep = false;
                    break;
                }
                PipelineOutcome::LimitReached => return Ok(()),
            }
        }

        if keep {
            on_row(row.expect("row is present after pipeline"))?;
            emitted += 1;
            if limit.is_some_and(|limit| emitted >= limit) {
                return Ok(());
            }
        }
    }

    Ok(())
}

/// Stream Docker network events for a compose project in real-time.
/// Streams Docker events, filters by `event_type == "network"` and the compose
/// project label, and applies the compose query's pipeline to each row.
pub async fn stream_compose_networks<C, F>(
    query: &ComposeQuery,
    docker: Arc<C>,
    mut on_row: F,
) -> Result<(), EventsError>
where
    C: DockerClient + Send + Sync + 'static,
    F: FnMut(Row) -> Result<(), EventsError>,
{
    let label = format!("com.docker.compose.project={}", query.project);
    let pipeline = &query.pipeline;

    let limit = pipeline.iter().find_map(|node| match node {
        PipelineNode::Limit(limit) => Some(*limit),
        _ => None,
    });

    let mut stream = docker.events_stream(None, None).await?;
    let mut emitted = 0u64;

    while let Some(item) = stream.next().await {
        let event = item?;

        // Only network events
        if event.event_type != "network" {
            continue;
        }

        // Filter by compose project label
        let has_project = event
            .attributes
            .iter()
            .any(|(key, value)| format!("{key}={value}").contains(&label));
        if !has_project {
            continue;
        }

        let mut row = Some(event_row(event));

        let mut keep = true;
        for node in pipeline {
            let current = row.take().expect("row is present while pipeline runs");
            match apply_pipeline_node(current, node)? {
                PipelineOutcome::Row(next_row) => row = Some(next_row),
                PipelineOutcome::Drop => {
                    keep = false;
                    break;
                }
                PipelineOutcome::LimitReached => return Ok(()),
            }
        }

        if keep {
            on_row(row.expect("row is present after pipeline"))?;
            emitted += 1;
            if limit.is_some_and(|limit| emitted >= limit) {
                return Ok(());
            }
        }
    }

    Ok(())
}

/// Stream log lines in real-time, applying the query pipeline to each row.
pub async fn stream_logs<S, F>(
    query: &LogsQuery,
    source: &S,
    mut on_row: F,
) -> Result<(), EventsError>
where
    S: LogSource + ?Sized + Sync,
    F: FnMut(Row) -> Result<(), EventsError>,
{
    let tail = query.tail.unwrap_or(100) as usize;
    let limit = query.pipeline.iter().find_map(|node| match node {
        PipelineNode::Limit(limit) => Some(*limit),
        _ => None,
    });

    let mut emitted = 0u64;
    let mut line_num = 0u64;
    let mut stream = source.logs_stream(&query.container, tail).await?;
    while let Some(line) = stream.next().await {
        line_num += 1;
        let mut row = Some(log_row(&query.container, line_num, line?));

        if let Some(filter) = &query.filter
            && !eval::evaluate_expression(&row.as_ref().expect("row is present").fields, filter)?
        {
            continue;
        }

        let mut keep = true;
        for node in &query.pipeline {
            let current = row.take().expect("row is present while pipeline runs");
            match apply_pipeline_node(current, node)? {
                PipelineOutcome::Row(next_row) => row = Some(next_row),
                PipelineOutcome::Drop => {
                    keep = false;
                    break;
                }
                PipelineOutcome::LimitReached => return Ok(()),
            }
        }

        if keep {
            on_row(row.expect("row is present after pipeline"))?;
            emitted += 1;
            if limit.is_some_and(|limit| emitted >= limit) {
                return Ok(());
            }
        }
    }

    Ok(())
}

pub async fn collect_events<S>(
    query: &EventsQuery,
    source: &S,
) -> Result<ExecutionResult, EventsError>
where
    S: EventSource + ?Sized + Sync,
{
    ensure_supported_target(query.target)?;

    let mut rows = Vec::new();
    let limit = query.pipeline.iter().find_map(|node| match node {
        PipelineNode::Limit(limit) => Some(*limit),
        _ => None,
    });

    let mut stream = source.events_stream().await?;
    while let Some(event) = stream.next().await {
        let mut row = Some(event_row(event?));

        if let Some(filter) = &query.filter
            && !eval::evaluate_expression(&row.as_ref().expect("row is present").fields, filter)?
        {
            continue;
        }

        let mut keep = true;
        for node in &query.pipeline {
            let current = row.take().expect("row is present while pipeline runs");
            match apply_pipeline_node(current, node)? {
                PipelineOutcome::Row(next_row) => row = Some(next_row),
                PipelineOutcome::Drop => {
                    keep = false;
                    break;
                }
                PipelineOutcome::LimitReached => {
                    return Ok(ExecutionResult { rows });
                }
            }
        }

        if keep {
            rows.push(row.expect("row is present after pipeline"));
            if limit.is_some_and(|limit| rows.len() as u64 >= limit) {
                break;
            }
        }
    }

    Ok(ExecutionResult { rows })
}

pub async fn stream_events<S, F>(
    query: &EventsQuery,
    source: &S,
    mut on_row: F,
) -> Result<(), EventsError>
where
    S: EventSource + ?Sized + Sync,
    F: FnMut(Row) -> Result<(), EventsError>,
{
    ensure_supported_target(query.target)?;

    let has_group_by = query
        .pipeline
        .iter()
        .any(|n| matches!(n, PipelineNode::GroupBy { .. }));
    let group_fields: Option<Vec<String>> = query.pipeline.iter().find_map(|n| {
        if let PipelineNode::GroupBy { fields, .. } = n {
            Some(fields.clone())
        } else {
            None
        }
    });

    let mut emitted = 0_u64;
    let limit = query.pipeline.iter().find_map(|node| match node {
        PipelineNode::Limit(limit) => Some(*limit),
        _ => None,
    });

    let mut group_counts: std::collections::BTreeMap<Vec<String>, u64> =
        std::collections::BTreeMap::new();
    let mut group_rows: std::collections::BTreeMap<Vec<String>, Row> =
        std::collections::BTreeMap::new();
    let mut rows_since_flush = 0_u64;

    let mut stream = source.events_stream().await?;
    while let Some(event) = stream.next().await {
        let mut row = Some(event_row(event?));

        if let Some(filter) = &query.filter
            && !eval::evaluate_expression(&row.as_ref().expect("row is present").fields, filter)?
        {
            continue;
        }

        let mut keep = true;
        for node in &query.pipeline {
            let current = row.take().expect("row is present while pipeline runs");

            if has_group_by {
                match node {
                    PipelineNode::GroupBy { fields, .. } => {
                        let key: Vec<String> = fields
                            .iter()
                            .map(|f| {
                                current
                                    .fields
                                    .get(f)
                                    .map(eval::render_json_cell)
                                    .unwrap_or_default()
                            })
                            .collect();

                        let count = group_counts.entry(key.clone()).or_insert(0);
                        *count += 1;

                        if !group_rows.contains_key(&key) {
                            let mut row_fields = BTreeMap::new();
                            for f in fields {
                                if let Some(v) = current.fields.get(f) {
                                    row_fields.insert(f.clone(), v.clone());
                                }
                            }
                            group_rows.insert(key.clone(), Row { fields: row_fields });
                        }

                        row = None;
                        keep = false;
                        rows_since_flush += 1;

                        if rows_since_flush >= 50 {
                            flush_grouped(
                                group_fields.as_ref(),
                                &mut group_counts,
                                &mut group_rows,
                                &mut on_row,
                                &mut emitted,
                                limit,
                            )?;
                            rows_since_flush = 0;
                            if limit.is_some_and(|l| emitted >= l) {
                                return Ok(());
                            }
                        }
                        break;
                    }
                    _ => {
                        row = match apply_pipeline_node(current, node)? {
                            PipelineOutcome::Row(next_row) => Some(next_row),
                            PipelineOutcome::Drop => {
                                keep = false;
                                break;
                            }
                            PipelineOutcome::LimitReached => {
                                flush_grouped(
                                    group_fields.as_ref(),
                                    &mut group_counts,
                                    &mut group_rows,
                                    &mut on_row,
                                    &mut emitted,
                                    limit,
                                )?;
                                return Ok(());
                            }
                        };
                    }
                }
            } else {
                match apply_pipeline_node(current, node)? {
                    PipelineOutcome::Row(next_row) => row = Some(next_row),
                    PipelineOutcome::Drop => {
                        keep = false;
                        break;
                    }
                    PipelineOutcome::LimitReached => return Ok(()),
                }
            }
        }

        if keep && !has_group_by {
            on_row(row.expect("row is present after pipeline"))?;
            emitted += 1;
            if limit.is_some_and(|limit| emitted >= limit) {
                return Ok(());
            }
        }
    }

    if has_group_by {
        flush_grouped(
            group_fields.as_ref(),
            &mut group_counts,
            &mut group_rows,
            &mut on_row,
            &mut emitted,
            limit,
        )?;
    }

    Ok(())
}

fn flush_grouped<F>(
    _group_fields: Option<&Vec<String>>,
    group_counts: &mut std::collections::BTreeMap<Vec<String>, u64>,
    group_rows: &mut std::collections::BTreeMap<Vec<String>, Row>,
    on_row: &mut F,
    emitted: &mut u64,
    limit: Option<u64>,
) -> Result<(), EventsError>
where
    F: FnMut(Row) -> Result<(), EventsError>,
{
    let keys: Vec<Vec<String>> = group_counts.keys().cloned().collect();
    for key in keys {
        if let Some(count) = group_counts.remove(&key)
            && let Some(mut row) = group_rows.remove(&key)
        {
            row.fields
                .insert("count".to_owned(), serde_json::json!(count));
            on_row(row)?;
            *emitted += 1;
            if limit.is_some_and(|l| *emitted >= l) {
                break;
            }
        }
    }
    Ok(())
}

#[must_use]
pub fn event_row(event: DockerEvent) -> Row {
    Row {
        fields: BTreeMap::from([
            ("time".to_owned(), json_string(event.time)),
            ("type".to_owned(), json_string(event.event_type)),
            ("action".to_owned(), json_string(event.action)),
            ("actor_id".to_owned(), json_string(event.actor_id)),
            ("container".to_owned(), json_option_string(event.container)),
            ("image".to_owned(), json_option_string(event.image)),
            (
                "attributes".to_owned(),
                JsonValue::Array(
                    event
                        .attributes
                        .into_iter()
                        .map(|(key, value)| JsonValue::String(format!("{key}={value}")))
                        .collect(),
                ),
            ),
        ]),
    }
}

pub fn parse_docker_event_json(line: &str) -> Result<DockerEvent, EventsError> {
    let value = serde_json::from_str::<JsonValue>(line).map_err(EventsError::Json)?;
    let attributes = value
        .get("Actor")
        .and_then(|actor| actor.get("Attributes"))
        .and_then(JsonValue::as_object)
        .map(|attributes| {
            attributes
                .iter()
                .map(|(key, value)| (key.clone(), json_value_to_string(value)))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let container =
        attribute(&attributes, "name").or_else(|| string(&value, &["container", "ActorID"]));
    let image = attribute(&attributes, "image").or_else(|| string(&value, &["from"]));

    Ok(DockerEvent {
        time: string(&value, &["time", "timeNano", "Time", "TimeNano"]).unwrap_or_default(),
        event_type: string(&value, &["Type", "type"]).unwrap_or_default(),
        action: string(&value, &["Action", "action", "status"]).unwrap_or_default(),
        actor_id: value
            .get("Actor")
            .and_then(|actor| actor.get("ID"))
            .and_then(JsonValue::as_str)
            .map(str::to_owned)
            .or_else(|| string(&value, &["id", "ActorID"]))
            .unwrap_or_default(),
        container,
        image,
        attributes,
    })
}

enum PipelineOutcome {
    Row(Row),
    Drop,
    LimitReached,
}

fn apply_pipeline_node(row: Row, node: &PipelineNode) -> Result<PipelineOutcome, EventsError> {
    match node {
        PipelineNode::Where(expression) | PipelineNode::Having(expression) => {
            if eval::evaluate_expression(&row.fields, expression)? {
                Ok(PipelineOutcome::Row(row))
            } else {
                Ok(PipelineOutcome::Drop)
            }
        }
        PipelineNode::Select(fields) => select_fields(&row, fields).map(PipelineOutcome::Row),
        PipelineNode::Limit(0) => Ok(PipelineOutcome::LimitReached),
        PipelineNode::Limit(_) => Ok(PipelineOutcome::Row(row)),
        PipelineNode::GroupBy { .. } => {
            // In streaming, GroupBy is handled at the stream level.
            Ok(PipelineOutcome::Row(row))
        }
        PipelineNode::SortBy { .. } => Err(EventsError::UnsupportedPipeline("sort by")),
        PipelineNode::Distinct => Err(EventsError::UnsupportedPipeline("distinct")),
        PipelineNode::Offset(_) => Err(EventsError::UnsupportedPipeline("offset")),
        PipelineNode::Alert(message) => {
            eprintln!(
                "[ALERT] {message}: {}",
                row.fields
                    .iter()
                    .map(|(k, v)| format!("{k}={}", eval::render_json_cell(v)))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            Ok(PipelineOutcome::Row(row))
        }
        PipelineNode::Set { field, value } => {
            let mut row = row;
            let json_value = eval::evaluate_set_value(&row.fields, value)?;
            row.fields.insert(field.clone(), json_value);
            Ok(PipelineOutcome::Row(row))
        }
        PipelineNode::Fill {
            field,
            default,
            condition,
        } => {
            let mut row = row;
            // Check condition first (if present)
            if let Some(cond) = condition
                && !eval::evaluate_expression(&row.fields, cond)?
            {
                return Ok(PipelineOutcome::Row(row));
            }
            if !row.fields.contains_key(field)
                || row.fields.get(field) == Some(&JsonValue::Null)
                || matches!(row.fields.get(field), Some(JsonValue::String(s)) if s.is_empty())
            {
                let value = eval::evaluate_set_value(&row.fields, default)?;
                row.fields.insert(field.clone(), value);
            }
            Ok(PipelineOutcome::Row(row))
        }
        PipelineNode::Debug => {
            eprintln!(
                "[debug] event: {}={}, action={}",
                row.fields
                    .get("type")
                    .map(eval::render_json_cell)
                    .unwrap_or_default(),
                row.fields
                    .get("container")
                    .or_else(|| row.fields.get("actor_id"))
                    .map(eval::render_json_cell)
                    .unwrap_or_default(),
                row.fields
                    .get("action")
                    .map(eval::render_json_cell)
                    .unwrap_or_default(),
            );
            Ok(PipelineOutcome::Row(row))
        }
        PipelineNode::RowNumber { .. } => Err(EventsError::UnsupportedPipeline("row_number")),
        PipelineNode::Rank { .. } => Err(EventsError::UnsupportedPipeline("rank")),
        PipelineNode::Lag { .. } => Err(EventsError::UnsupportedPipeline("lag")),
        PipelineNode::Lead { .. } => Err(EventsError::UnsupportedPipeline("lead")),
        PipelineNode::Assert(condition) => {
            if eval::evaluate_expression(&row.fields, condition)? {
                Ok(PipelineOutcome::Row(row))
            } else {
                let details = row
                    .fields
                    .iter()
                    .map(|(k, v)| format!("{k}={}", eval::render_json_cell(v)))
                    .collect::<Vec<_>>()
                    .join(", ");
                Err(EventsError::AssertionFailed(format!(
                    "assertion failed for row: {details}"
                )))
            }
        }
        PipelineNode::Let { name, value } => {
            let mut row = row;
            let value = eval::eval_expr(&BTreeMap::new(), value)?;
            row.fields.insert(name.clone(), value);
            Ok(PipelineOutcome::Row(row))
        }
        PipelineNode::If {
            condition,
            then_branch,
            else_branch,
        } => {
            let empty = Vec::new();
            let matched = eval::evaluate_expression(&row.fields, condition)?;
            let branch = if matched {
                then_branch
            } else {
                else_branch.as_ref().unwrap_or(&empty)
            };
            if branch.is_empty() {
                Ok(PipelineOutcome::Row(row))
            } else {
                apply_inline_pipeline(row, branch)
            }
        }
    }
}

fn select_fields(row: &Row, fields: &[String]) -> Result<Row, EventsError> {
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

fn apply_inline_pipeline(
    mut row: Row,
    nodes: &[PipelineNode],
) -> Result<PipelineOutcome, EventsError> {
    for node in nodes {
        match apply_pipeline_node(row, node)? {
            PipelineOutcome::Row(next) => row = next,
            PipelineOutcome::Drop => return Ok(PipelineOutcome::Drop),
            PipelineOutcome::LimitReached => return Ok(PipelineOutcome::LimitReached),
        }
    }
    Ok(PipelineOutcome::Row(row))
}

#[allow(clippy::unnecessary_wraps)]
const fn ensure_supported_target(target: CollectionTarget) -> Result<(), EventsError> {
    let _ = target;
    Ok(())
}

fn string(value: &JsonValue, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key))
        .map(json_value_to_string)
        .filter(|value| !value.is_empty())
}

fn attribute(attributes: &[(String, String)], key: &str) -> Option<String> {
    attributes
        .iter()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, value)| value.clone())
}

fn json_value_to_string(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => String::new(),
        JsonValue::String(value) => value.clone(),
        other => other.to_string(),
    }
}

fn json_option_string(value: Option<String>) -> JsonValue {
    value.map_or(JsonValue::Null, JsonValue::String)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ast::{ComposeTarget, Query},
        docker::{Container, MockDockerClient},
        parser,
    };

    #[test]
    fn parses_docker_event_json() {
        let event = parse_docker_event_json(
            r#"{"time":1717120800,"Type":"container","Action":"die","Actor":{"ID":"abc","Attributes":{"name":"api","image":"api:latest","exitCode":"1"}}}"#,
        )
        .expect("event should parse");

        assert_eq!(event.event_type, "container");
        assert_eq!(event.action, "die");
        assert_eq!(event.actor_id, "abc");
        assert_eq!(event.container.as_deref(), Some("api"));
        assert_eq!(event.image.as_deref(), Some("api:latest"));
    }

    #[test]
    fn filters_mock_event_stream() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let Query::Events(query) = parser::parse(
            "events containers where action = \"die\" | select time, container, action",
        )
        .expect("query should parse")
        .query
        else {
            panic!("expected events query");
        };
        let source = MockEventSource {
            events: vec![
                event("start", "api"),
                event("die", "api"),
                event("restart", "worker"),
            ],
        };

        let result = rt
            .block_on(collect_events(&query, &source))
            .expect("events should collect");

        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["container"],
            JsonValue::String("api".to_owned())
        );
        assert_eq!(
            result.rows[0].fields.keys().cloned().collect::<Vec<_>>(),
            vec![
                "action".to_owned(),
                "container".to_owned(),
                "time".to_owned()
            ]
        );
    }

    #[test]
    fn honors_stream_limit() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let Query::Events(query) = parser::parse("events containers | limit 1")
            .expect("query should parse")
            .query
        else {
            panic!("expected events query");
        };
        let source = MockEventSource {
            events: vec![event("start", "api"), event("die", "api")],
        };

        let result = rt
            .block_on(collect_events(&query, &source))
            .expect("events should collect");

        assert_eq!(result.rows.len(), 1);
    }

    #[test]
    fn supports_non_container_event_targets() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let Query::Events(query) = parser::parse("events images where action = \"pull\"")
            .expect("query should parse")
            .query
        else {
            panic!("expected events query");
        };
        let source = MockEventSource {
            events: vec![
                DockerEvent {
                    time: "2026-05-31T02:00:00Z".to_owned(),
                    event_type: "image".to_owned(),
                    action: "pull".to_owned(),
                    actor_id: "user".to_owned(),
                    container: None,
                    image: Some("postgres:16".to_owned()),
                    attributes: Vec::new(),
                },
                DockerEvent {
                    time: "2026-05-31T02:00:00Z".to_owned(),
                    event_type: "image".to_owned(),
                    action: "delete".to_owned(),
                    actor_id: "user".to_owned(),
                    container: None,
                    image: Some("old:latest".to_owned()),
                    attributes: Vec::new(),
                },
            ],
        };

        let result = rt
            .block_on(collect_events(&query, &source))
            .expect("events should collect");

        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["image"],
            JsonValue::String("postgres:16".to_owned())
        );
    }

    // ── stream_events callback tests ────────────────────────────────────

    #[test]
    fn stream_events_filter_with_callback() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let Query::Events(query) = parser::parse(
            "events containers where action = \"die\" | select time, container, action | limit 1",
        )
        .expect("query should parse")
        .query
        else {
            panic!("expected events query");
        };
        let source = MockEventSource {
            events: vec![
                event("start", "api"),
                event("die", "api"),
                event("restart", "worker"),
            ],
        };

        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let cb = std::sync::Arc::clone(&received);

        rt.block_on(stream_events(&query, &source, move |row| {
            cb.lock().unwrap().push(row);
            Ok(())
        }))
        .expect("events should stream");

        let rows = received.lock().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].fields["container"],
            JsonValue::String("api".to_owned())
        );
        assert_eq!(
            rows[0].fields.keys().cloned().collect::<Vec<_>>(),
            vec![
                "action".to_owned(),
                "container".to_owned(),
                "time".to_owned()
            ]
        );
    }

    // ── collect_logs tests ───────────────────────────────────────────────

    #[test]
    fn collect_logs_applies_filter() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let Query::Logs(query) =
            parser::parse("logs container test-container | where message contains \"error\"")
                .expect("query should parse")
                .query
        else {
            panic!("expected logs query");
        };
        let source = MockLogSource {
            lines: vec![
                "info: starting up".to_owned(),
                "error: connection refused".to_owned(),
                "warn: retrying".to_owned(),
                "error: timeout".to_owned(),
            ],
        };

        let result = rt
            .block_on(collect_logs(&query, &source))
            .expect("logs should collect");

        assert_eq!(result.rows.len(), 2);
        for row in &result.rows {
            let msg = row.fields["message"].as_str().unwrap();
            assert!(msg.contains("error"), "expected error line, got: {msg}");
        }
    }

    #[test]
    fn collect_logs_applies_limit() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let Query::Logs(query) = parser::parse("logs container test-container tail 100 | limit 2")
            .expect("query should parse")
            .query
        else {
            panic!("expected logs query");
        };
        let source = MockLogSource {
            lines: vec![
                "line1".to_owned(),
                "line2".to_owned(),
                "line3".to_owned(),
                "line4".to_owned(),
            ],
        };

        let result = rt
            .block_on(collect_logs(&query, &source))
            .expect("logs should collect");

        assert_eq!(result.rows.len(), 2);
        assert_eq!(
            result.rows[0].fields["message"],
            JsonValue::String("line1".to_owned())
        );
        assert_eq!(
            result.rows[1].fields["message"],
            JsonValue::String("line2".to_owned())
        );
    }

    #[test]
    fn collect_logs_applies_select() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let Query::Logs(query) = parser::parse("logs container test-container | select message")
            .expect("query should parse")
            .query
        else {
            panic!("expected logs query");
        };
        let source = MockLogSource {
            lines: vec!["hello".to_owned(), "world".to_owned()],
        };

        let result = rt
            .block_on(collect_logs(&query, &source))
            .expect("logs should collect");

        assert_eq!(result.rows.len(), 2);
        // Only the selected field should be present
        assert!(
            !result.rows[0].fields.contains_key("line"),
            "line field should be filtered out by select"
        );
        assert!(
            !result.rows[0].fields.contains_key("container"),
            "container field should be filtered out by select"
        );
        // Positive check: selected field must be present with correct value
        assert_eq!(
            result.rows[0].fields["message"],
            JsonValue::String("hello".to_owned())
        );
        assert_eq!(
            result.rows[1].fields["message"],
            JsonValue::String("world".to_owned())
        );
    }

    // ── stream_logs callback tests ───────────────────────────────────────

    #[test]
    fn stream_logs_with_callback() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let Query::Logs(query) = parser::parse("logs container test-container tail 10")
            .expect("query should parse")
            .query
        else {
            panic!("expected logs query");
        };
        let source = MockLogSource {
            lines: vec!["hello".to_owned(), "world".to_owned()],
        };

        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let cb = std::sync::Arc::clone(&received);

        rt.block_on(stream_logs(&query, &source, move |row| {
            cb.lock().unwrap().push(row);
            Ok(())
        }))
        .expect("logs should stream");

        let rows = received.lock().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].fields["message"],
            JsonValue::String("hello".to_owned())
        );
        assert_eq!(
            rows[1].fields["message"],
            JsonValue::String("world".to_owned())
        );
        // Verify all expected fields are present
        assert_eq!(rows[0].fields["line"], JsonValue::Number(Number::from(1)));
        assert_eq!(rows[1].fields["line"], JsonValue::Number(Number::from(2)));
        assert_eq!(
            rows[0].fields["container"],
            JsonValue::String("test-container".to_owned())
        );
    }

    // ── stream_compose_networks tests ──────────────────────────────────────

    fn net_event(action: &str, project: &str) -> DockerEvent {
        DockerEvent {
            time: "2026-05-31T02:00:00Z".to_owned(),
            event_type: "network".to_owned(),
            action: action.to_owned(),
            actor_id: "net-actor".to_owned(),
            container: None,
            image: None,
            attributes: vec![
                ("com.docker.compose.project".to_owned(), project.to_owned()),
                ("name".to_owned(), format!("{project}_default")),
            ],
        }
    }

    #[test]
    fn stream_compose_networks_basic() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let docker = Arc::new(MockDockerClient {
            events: vec![
                net_event("create", "myapp"),
                net_event("connect", "myapp"),
                net_event("create", "other"), // different project, should be filtered out
            ],
            ..MockDockerClient::default()
        });

        let query = ComposeQuery {
            project: "myapp".to_owned(),
            target: ComposeTarget::Networks,
            service: None,
            tail: None,
            pipeline: Vec::new(),
            port_number: None,
            config_target: None,
        };

        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let cb = std::sync::Arc::clone(&received);

        rt.block_on(stream_compose_networks(&query, docker, move |row| {
            cb.lock().unwrap().push(row);
            Ok(())
        }))
        .expect("compose networks should stream");

        let rows = received.lock().unwrap();
        assert_eq!(rows.len(), 2, "expected 2 network events for myapp");

        let actions: Vec<&str> = rows
            .iter()
            .map(|r| r.fields["action"].as_str().unwrap())
            .collect();
        assert!(actions.contains(&"create"));
        assert!(actions.contains(&"connect"));

        // Verify type field is "network" for all rows
        for row in rows.iter() {
            assert_eq!(
                row.fields["type"].as_str().unwrap(),
                "network",
                "event type should be 'network'"
            );
        }
    }

    #[test]
    fn stream_compose_networks_filters_non_network_events() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let docker = Arc::new(MockDockerClient {
            events: vec![
                DockerEvent {
                    time: "2026-05-31T02:00:00Z".to_owned(),
                    event_type: "container".to_owned(),
                    action: "start".to_owned(),
                    actor_id: "c1".to_owned(),
                    container: Some("api".to_owned()),
                    image: Some("api:latest".to_owned()),
                    attributes: vec![
                        ("com.docker.compose.project".to_owned(), "myapp".to_owned()),
                        ("com.docker.compose.service".to_owned(), "api".to_owned()),
                    ],
                },
                net_event("connect", "myapp"),
            ],
            ..MockDockerClient::default()
        });

        let query = ComposeQuery {
            project: "myapp".to_owned(),
            target: ComposeTarget::Networks,
            service: None,
            tail: None,
            pipeline: Vec::new(),
            port_number: None,
            config_target: None,
        };

        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let cb = std::sync::Arc::clone(&received);

        rt.block_on(stream_compose_networks(&query, docker, move |row| {
            cb.lock().unwrap().push(row);
            Ok(())
        }))
        .expect("compose networks should stream");

        let rows = received.lock().unwrap();
        assert_eq!(
            rows.len(),
            1,
            "expected only 1 network event (container event filtered out)"
        );
        assert_eq!(rows[0].fields["action"].as_str().unwrap(), "connect");
    }

    #[test]
    fn stream_compose_networks_select_action() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let docker = Arc::new(MockDockerClient {
            events: vec![net_event("create", "myapp"), net_event("connect", "myapp")],
            ..MockDockerClient::default()
        });

        let query = ComposeQuery {
            project: "myapp".to_owned(),
            target: ComposeTarget::Networks,
            service: None,
            tail: None,
            pipeline: vec![PipelineNode::Select(vec!["action".to_owned()])],
            port_number: None,
            config_target: None,
        };

        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let cb = std::sync::Arc::clone(&received);

        rt.block_on(stream_compose_networks(&query, docker, move |row| {
            cb.lock().unwrap().push(row);
            Ok(())
        }))
        .expect("compose networks should stream");

        let rows = received.lock().unwrap();
        assert_eq!(rows.len(), 2);
        // Only action field should be present
        assert!(rows[0].fields.contains_key("action"));
        assert!(!rows[0].fields.contains_key("type"));
        assert!(!rows[0].fields.contains_key("time"));
        assert!(!rows[0].fields.contains_key("actor_id"));
        assert_eq!(rows[0].fields["action"].as_str().unwrap(), "create");
        assert_eq!(rows[1].fields["action"].as_str().unwrap(), "connect");
    }

    #[test]
    fn stream_compose_networks_with_limit() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let docker = Arc::new(MockDockerClient {
            events: vec![
                net_event("create", "myapp"),
                net_event("connect", "myapp"),
                net_event("remove", "myapp"),
            ],
            ..MockDockerClient::default()
        });

        let query = ComposeQuery {
            project: "myapp".to_owned(),
            target: ComposeTarget::Networks,
            service: None,
            tail: None,
            pipeline: vec![PipelineNode::Limit(2)],
            port_number: None,
            config_target: None,
        };

        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let cb = std::sync::Arc::clone(&received);

        rt.block_on(stream_compose_networks(&query, docker, move |row| {
            cb.lock().unwrap().push(row);
            Ok(())
        }))
        .expect("compose networks should stream");

        let rows = received.lock().unwrap();
        assert_eq!(rows.len(), 2, "expected 2 rows (limit 2)");
    }

    // ── stream_compose_logs tests ─────────────────────────────────────────

    fn compose_container(id: &str, name: &str, project: &str, service: &str) -> Container {
        Container {
            id: id.to_owned(),
            name: name.to_owned(),
            labels: vec![
                format!("com.docker.compose.project={project}"),
                format!("com.docker.compose.service={service}"),
            ],
            state: "running".to_owned(),
            ..Container::default()
        }
    }

    #[test]
    fn stream_compose_logs_basic() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let docker = Arc::new(MockDockerClient {
            containers: vec![
                compose_container("c1", "api-1", "myapp", "api"),
                compose_container("c2", "api-2", "myapp", "api"),
            ],
            logs: {
                let mut m = std::collections::HashMap::new();
                m.insert("c1".to_owned(), vec!["log1".to_owned(), "log2".to_owned()]);
                m.insert("c2".to_owned(), vec!["log3".to_owned(), "log4".to_owned()]);
                m
            },
            ..MockDockerClient::default()
        });

        let query = ComposeQuery {
            project: "myapp".to_owned(),
            target: ComposeTarget::Logs,
            service: Some("api".to_owned()),
            tail: Some(100),
            pipeline: Vec::new(),
            port_number: None,
            config_target: None,
        };

        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let cb = std::sync::Arc::clone(&received);

        rt.block_on(stream_compose_logs(&query, docker, move |row| {
            cb.lock().unwrap().push(row);
            Ok(())
        }))
        .expect("compose logs should stream");

        let rows = received.lock().unwrap();
        // 2 containers × 2 lines each = 4 rows (order depends on select_all interleaving)
        assert_eq!(rows.len(), 4, "expected 4 log lines from 2 containers");

        // Verify all expected messages appear
        let msgs: Vec<&str> = rows
            .iter()
            .map(|r| r.fields["message"].as_str().unwrap())
            .collect();
        assert!(msgs.contains(&"log1"));
        assert!(msgs.contains(&"log2"));
        assert!(msgs.contains(&"log3"));
        assert!(msgs.contains(&"log4"));

        // Verify container names are correct
        for row in rows.iter() {
            let container = row.fields["container"].as_str().unwrap();
            assert!(
                container == "api-1" || container == "api-2",
                "unexpected container: {container}"
            );
        }

        // Verify service field is present
        for row in rows.iter() {
            assert_eq!(
                row.fields["service"].as_str().unwrap(),
                "api",
                "service field should be 'api'"
            );
        }
    }

    #[test]
    fn stream_compose_logs_select_message() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let docker = Arc::new(MockDockerClient {
            containers: vec![compose_container("c1", "api-1", "myapp", "api")],
            logs: {
                let mut m = std::collections::HashMap::new();
                m.insert("c1".to_owned(), vec!["hello".to_owned()]);
                m
            },
            ..MockDockerClient::default()
        });

        let query = ComposeQuery {
            project: "myapp".to_owned(),
            target: ComposeTarget::Logs,
            service: Some("api".to_owned()),
            tail: Some(100),
            pipeline: vec![PipelineNode::Select(vec!["message".to_owned()])],
            port_number: None,
            config_target: None,
        };

        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let cb = std::sync::Arc::clone(&received);

        rt.block_on(stream_compose_logs(&query, docker, move |row| {
            cb.lock().unwrap().push(row);
            Ok(())
        }))
        .expect("compose logs should stream");

        let rows = received.lock().unwrap();
        assert_eq!(rows.len(), 1, "expected 1 row from 1 container");

        // Only message field should be present
        assert!(
            rows[0].fields.contains_key("message"),
            "message field should be present"
        );
        assert_eq!(
            rows[0].fields["message"],
            JsonValue::String("hello".to_owned())
        );
        // Non-selected fields should be absent
        assert!(
            !rows[0].fields.contains_key("container"),
            "container should be filtered out by select"
        );
        assert!(
            !rows[0].fields.contains_key("service"),
            "service should be filtered out by select"
        );
    }

    #[test]
    fn stream_compose_logs_no_matching_containers() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let docker = Arc::new(MockDockerClient {
            containers: vec![compose_container("c1", "api-1", "other", "api")],
            logs: {
                let mut m = std::collections::HashMap::new();
                m.insert("c1".to_owned(), vec!["log1".to_owned()]);
                m
            },
            ..MockDockerClient::default()
        });

        let query = ComposeQuery {
            project: "myapp".to_owned(),
            target: ComposeTarget::Logs,
            service: Some("api".to_owned()),
            tail: Some(100),
            pipeline: Vec::new(),
            port_number: None,
            config_target: None,
        };

        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let cb = std::sync::Arc::clone(&received);

        rt.block_on(stream_compose_logs(&query, docker, move |row| {
            cb.lock().unwrap().push(row);
            Ok(())
        }))
        .expect("compose logs should stream");

        let rows = received.lock().unwrap();
        assert_eq!(rows.len(), 0, "expected 0 rows when no containers match");
    }

    #[test]
    fn stream_compose_logs_with_limit() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let docker = Arc::new(MockDockerClient {
            containers: vec![compose_container("c1", "api-1", "myapp", "api")],
            logs: {
                let mut m = std::collections::HashMap::new();
                m.insert(
                    "c1".to_owned(),
                    vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
                );
                m
            },
            ..MockDockerClient::default()
        });

        let query = ComposeQuery {
            project: "myapp".to_owned(),
            target: ComposeTarget::Logs,
            service: Some("api".to_owned()),
            tail: Some(100),
            pipeline: vec![PipelineNode::Limit(2)],
            port_number: None,
            config_target: None,
        };

        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let cb = std::sync::Arc::clone(&received);

        rt.block_on(stream_compose_logs(&query, docker, move |row| {
            cb.lock().unwrap().push(row);
            Ok(())
        }))
        .expect("compose logs should stream");

        let rows = received.lock().unwrap();
        assert_eq!(rows.len(), 2, "expected 2 rows (limit 2)");
    }

    fn event(action: &str, container: &str) -> DockerEvent {
        DockerEvent {
            time: "2026-05-31T02:00:00Z".to_owned(),
            event_type: "container".to_owned(),
            action: action.to_owned(),
            actor_id: format!("{container}-id"),
            container: Some(container.to_owned()),
            image: Some(format!("{container}:latest")),
            attributes: vec![("name".to_owned(), container.to_owned())],
        }
    }
}
