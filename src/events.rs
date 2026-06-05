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

use std::{collections::BTreeMap, pin::Pin};

use futures_util::{Stream, StreamExt};
use serde_json::Value as JsonValue;
use thiserror::Error;

use crate::{
    ast::{CollectionTarget, EventsQuery, PipelineNode},
    docker::{DockerClient, DockerError, DockerEvent},
    eval::{self, EvalError},
    executor::{ExecutionResult, Row},
    json_string,
};

/// Shared type alias for the complex event stream return type.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<DockerEvent, EventsError>> + Send>>;

pub trait EventSource {
    fn events_stream(&self) -> impl std::future::Future<Output = Result<EventStream, EventsError>> + Send
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
    async fn events_stream(&self) -> Result<Pin<Box<dyn Stream<Item = Result<DockerEvent, EventsError>> + Send>>, EventsError> {
        let stream = self.docker.events_stream(None, None).await?;
        let mapped = stream.map(|result| result.map_err(EventsError::Docker));
        Ok(Box::pin(mapped))
    }
}

// ── MockEventSource ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct MockEventSource {
    pub events: Vec<DockerEvent>,
}

impl EventSource for MockEventSource {
    async fn events_stream(&self) -> Result<Pin<Box<dyn Stream<Item = Result<DockerEvent, EventsError>> + Send>>, EventsError> {
        let items: Vec<Result<DockerEvent, EventsError>> =
            self.events.clone().into_iter().map(Ok).collect();
        Ok(Box::pin(futures_util::stream::iter(items)))
    }
}

pub async fn collect_events<S>(query: &EventsQuery, source: &S) -> Result<ExecutionResult, EventsError>
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
        PipelineNode::Fill { field, default } => {
            let mut row = row;
            if !row.fields.contains_key(field)
                || row.fields.get(field) == Some(&JsonValue::Null)
                || matches!(row.fields.get(field), Some(JsonValue::String(s)) if s.is_empty())
            {
                let value = eval::eval_expr(&row.fields, default)?;
                row.fields.insert(field.clone(), value);
            }
            Ok(PipelineOutcome::Row(row))
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
    use crate::{ast::Query, parser};

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

        let result = rt.block_on(collect_events(&query, &source)).expect("events should collect");

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

        let result = rt.block_on(collect_events(&query, &source)).expect("events should collect");

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

        let result = rt.block_on(collect_events(&query, &source)).expect("events should collect");

        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["image"],
            JsonValue::String("postgres:16".to_owned())
        );
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
