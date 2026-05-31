use std::{
    collections::BTreeMap,
    ffi::OsStr,
    io::{BufRead, BufReader, Lines},
    process::{Child, ChildStdout, Command, Stdio},
};

use serde_json::Value as JsonValue;
use thiserror::Error;

use crate::{
    ast::{CollectionTarget, EventsQuery, PipelineNode},
    docker::{DockerError, DockerEvent},
    eval::{self, EvalError},
    executor::{ExecutionResult, Row},
};

pub trait EventSource {
    fn events(
        &self,
    ) -> Result<Box<dyn Iterator<Item = Result<DockerEvent, EventsError>>>, EventsError>;
}

#[derive(Debug, Error)]
pub enum EventsError {
    #[error("{0}")]
    Docker(#[from] DockerError),
    #[error("failed to run docker command `{command}`: {source}")]
    CommandIo {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read docker event stream: {0}")]
    Read(std::io::Error),
    #[error("failed to parse docker event JSON: {0}")]
    Json(serde_json::Error),
    #[error("unsupported event target: {0:?}")]
    UnsupportedTarget(CollectionTarget),
    #[error("unsupported event pipeline node: {0}")]
    UnsupportedPipeline(&'static str),
    #[error("{0}")]
    Eval(#[from] EvalError),
}

#[derive(Debug, Clone)]
pub struct DockerCliEventSource {
    docker_bin: String,
}

impl Default for DockerCliEventSource {
    fn default() -> Self {
        Self::new("docker")
    }
}

impl DockerCliEventSource {
    pub fn new(docker_bin: impl Into<String>) -> Self {
        Self {
            docker_bin: docker_bin.into(),
        }
    }

    fn spawn_events<I, S>(&self, args: I) -> Result<DockerEventLineIterator, EventsError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args = args.into_iter().collect::<Vec<_>>();
        let command_display = format_command(&self.docker_bin, &args);
        let mut child = Command::new(&self.docker_bin)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|source| EventsError::CommandIo {
                command: command_display,
                source,
            })?;
        let stdout = child.stdout.take().ok_or_else(|| EventsError::CommandIo {
            command: format_command(&self.docker_bin, &args),
            source: std::io::Error::other("missing docker stdout pipe"),
        })?;

        Ok(DockerEventLineIterator {
            _child: child,
            lines: BufReader::new(stdout).lines(),
        })
    }
}

impl EventSource for DockerCliEventSource {
    fn events(
        &self,
    ) -> Result<Box<dyn Iterator<Item = Result<DockerEvent, EventsError>>>, EventsError> {
        Ok(Box::new(self.spawn_events([
            "events",
            "--format",
            "{{json .}}",
        ])?))
    }
}

pub struct DockerEventLineIterator {
    _child: Child,
    lines: Lines<BufReader<ChildStdout>>,
}

impl Drop for DockerEventLineIterator {
    fn drop(&mut self) {
        let _ = self._child.kill();
        let _ = self._child.wait();
    }
}

impl Iterator for DockerEventLineIterator {
    type Item = Result<DockerEvent, EventsError>;

    fn next(&mut self) -> Option<Self::Item> {
        let line = match self.lines.next()? {
            Ok(line) => line,
            Err(error) => return Some(Err(EventsError::Read(error))),
        };
        Some(parse_docker_event_json(&line))
    }
}

#[derive(Debug, Clone, Default)]
pub struct MockEventSource {
    pub events: Vec<DockerEvent>,
}

impl EventSource for MockEventSource {
    fn events(
        &self,
    ) -> Result<Box<dyn Iterator<Item = Result<DockerEvent, EventsError>>>, EventsError> {
        Ok(Box::new(
            self.events
                .clone()
                .into_iter()
                .map(Ok::<DockerEvent, EventsError>),
        ))
    }
}

pub fn collect_events<S>(query: &EventsQuery, source: &S) -> Result<ExecutionResult, EventsError>
where
    S: EventSource + ?Sized,
{
    ensure_supported_target(query.target)?;

    let mut rows = Vec::new();
    let limit = query.pipeline.iter().find_map(|node| match node {
        PipelineNode::Limit(limit) => Some(*limit),
        _ => None,
    });

    for event in source.events()? {
        let mut row = Some(event_row(event?));

        if let Some(filter) = &query.filter {
            if !eval::evaluate_expression(&row.as_ref().expect("row is present").fields, filter)? {
                continue;
            }
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

pub fn stream_events<S, F>(
    query: &EventsQuery,
    source: &S,
    mut on_row: F,
) -> Result<(), EventsError>
where
    S: EventSource + ?Sized,
    F: FnMut(Row) -> Result<(), EventsError>,
{
    ensure_supported_target(query.target)?;

    let mut emitted = 0_u64;
    let limit = query.pipeline.iter().find_map(|node| match node {
        PipelineNode::Limit(limit) => Some(*limit),
        _ => None,
    });

    for event in source.events()? {
        let mut row = Some(event_row(event?));

        if let Some(filter) = &query.filter {
            if !eval::evaluate_expression(&row.as_ref().expect("row is present").fields, filter)? {
                continue;
            }
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
        PipelineNode::Where(expression) => {
            if eval::evaluate_expression(&row.fields, expression)? {
                Ok(PipelineOutcome::Row(row))
            } else {
                Ok(PipelineOutcome::Drop)
            }
        }
        PipelineNode::Select(fields) => select_fields(row, fields).map(PipelineOutcome::Row),
        PipelineNode::Limit(0) => Ok(PipelineOutcome::LimitReached),
        PipelineNode::Limit(_) => Ok(PipelineOutcome::Row(row)),
        PipelineNode::GroupBy(_) => Err(EventsError::UnsupportedPipeline("group by (not yet supported in streaming)")),
        PipelineNode::SortBy { .. } => Err(EventsError::UnsupportedPipeline("sort by")),
        PipelineNode::Alert(message) => {
            eprintln!(
                "[ALERT] {message}: {}",
                row
                    .fields
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

fn select_fields(row: Row, fields: &[String]) -> Result<Row, EventsError> {
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

fn ensure_supported_target(target: CollectionTarget) -> Result<(), EventsError> {
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

fn json_string(value: String) -> JsonValue {
    JsonValue::String(value)
}

fn json_option_string(value: Option<String>) -> JsonValue {
    value.map(JsonValue::String).unwrap_or(JsonValue::Null)
}

fn format_command<S>(bin: &str, args: &[S]) -> String
where
    S: AsRef<OsStr>,
{
    let mut parts = vec![bin.to_owned()];
    parts.extend(
        args.iter()
            .map(|arg| arg.as_ref().to_string_lossy().into_owned()),
    );
    parts.join(" ")
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

        let result = collect_events(&query, &source).expect("events should collect");

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
        let Query::Events(query) = parser::parse("events containers | limit 1")
            .expect("query should parse")
            .query
        else {
            panic!("expected events query");
        };
        let source = MockEventSource {
            events: vec![event("start", "api"), event("die", "api")],
        };

        let result = collect_events(&query, &source).expect("events should collect");

        assert_eq!(result.rows.len(), 1);
    }

    #[test]
    fn supports_non_container_event_targets() {
        let Query::Events(query) =
            parser::parse("events images where action = \"pull\"").expect("query should parse").query
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

        let result = collect_events(&query, &source).expect("events should collect");

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
