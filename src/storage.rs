use std::collections::BTreeMap;

use chrono::Utc;

use serde::{Deserialize, Serialize};
use serde_json::{Number, Value as JsonValue};
use thiserror::Error;

use crate::{
    ast::{EventsQuery, InspectQuery, SingularTargetKind, TimeSelector},
    docker::{Container, DockerEvent, Image, MetricSample, Network, Volume},
    events::{self, MockEventSource},
    executor::{ExecutionResult, Row},
};

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct TelemetrySnapshot {
    pub timestamp: String,
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
    pub networks: Vec<Network>,
    pub volumes: Vec<Volume>,
}

#[derive(Debug, Error)]
pub enum TelemetryError {
    #[error("historical query requires a telemetry store")]
    MissingStore,
    #[error("no snapshot found at or before `{timestamp}`")]
    SnapshotNotFound { timestamp: String },
    #[error("historical target not found: {target}")]
    TargetNotFound { target: String },
    #[error("{0}")]
    Events(#[from] events::EventsError),
    #[error("storage error: {0}")]
    Storage(String),
}

pub trait TelemetryStore {
    fn write_metric(&mut self, sample: MetricSample) -> Result<(), TelemetryError>;
    fn latest_metrics(&self) -> Result<Vec<MetricSample>, TelemetryError>;
    fn metrics_between(&self, from: &str, to: &str) -> Result<Vec<MetricSample>, TelemetryError>;
    fn write_event(&mut self, event: DockerEvent) -> Result<(), TelemetryError>;
    fn events_between(&self, from: &str, to: &str) -> Result<Vec<DockerEvent>, TelemetryError>;
    fn write_snapshot(&mut self, snapshot: TelemetrySnapshot) -> Result<(), TelemetryError>;
    fn snapshot_at_or_before(
        &self,
        timestamp: &str,
    ) -> Result<Option<TelemetrySnapshot>, TelemetryError>;
    fn write_alert_event(&mut self, event: AlertHistoryEvent) -> Result<(), TelemetryError>;
    fn alert_history(&self, from: &str, to: &str)
    -> Result<Vec<AlertHistoryEvent>, TelemetryError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertHistoryEvent {
    pub timestamp: String,
    pub container_id: String,
    pub container_name: String,
    pub rule_condition: String,
    pub action_type: String,
    pub action_detail: String,
    pub success: bool,
}

#[derive(Debug, Clone, Default)]
pub struct InMemoryTelemetryStore {
    metrics: Vec<MetricSample>,
    events: Vec<DockerEvent>,
    snapshots: Vec<TelemetrySnapshot>,
}

impl TelemetryStore for InMemoryTelemetryStore {
    fn write_metric(&mut self, sample: MetricSample) -> Result<(), TelemetryError> {
        self.metrics.push(sample);
        Ok(())
    }

    fn latest_metrics(&self) -> Result<Vec<MetricSample>, TelemetryError> {
        Ok(self.metrics.clone())
    }

    fn metrics_between(&self, from: &str, to: &str) -> Result<Vec<MetricSample>, TelemetryError> {
        Ok(self
            .metrics
            .iter()
            .filter(|sample| sample.timestamp.as_str() >= from && sample.timestamp.as_str() <= to)
            .cloned()
            .collect())
    }

    fn write_event(&mut self, event: DockerEvent) -> Result<(), TelemetryError> {
        self.events.push(event);
        Ok(())
    }

    fn events_between(&self, from: &str, to: &str) -> Result<Vec<DockerEvent>, TelemetryError> {
        Ok(self
            .events
            .iter()
            .filter(|event| event.time.as_str() >= from && event.time.as_str() <= to)
            .cloned()
            .collect())
    }

    fn write_snapshot(&mut self, snapshot: TelemetrySnapshot) -> Result<(), TelemetryError> {
        self.snapshots.push(snapshot);
        self.snapshots
            .sort_by(|left, right| left.timestamp.cmp(&right.timestamp));
        Ok(())
    }

    fn snapshot_at_or_before(
        &self,
        timestamp: &str,
    ) -> Result<Option<TelemetrySnapshot>, TelemetryError> {
        Ok(self
            .snapshots
            .iter()
            .filter(|snapshot| snapshot.timestamp.as_str() <= timestamp)
            .max_by(|left, right| left.timestamp.cmp(&right.timestamp))
            .cloned())
    }

    fn write_alert_event(&mut self, event: AlertHistoryEvent) -> Result<(), TelemetryError> {
        // InMemory store: no-op, silently accept
        let _ = event;
        Ok(())
    }

    fn alert_history(
        &self,
        _from: &str,
        _to: &str,
    ) -> Result<Vec<AlertHistoryEvent>, TelemetryError> {
        Ok(Vec::new())
    }
}

pub fn inspect_at<S>(query: &InspectQuery, store: &S) -> Result<ExecutionResult, TelemetryError>
where
    S: TelemetryStore + ?Sized,
{
    let timestamp = query.at.as_deref().ok_or(TelemetryError::MissingStore)?;
    let snapshot = store.snapshot_at_or_before(timestamp)?.ok_or_else(|| {
        TelemetryError::SnapshotNotFound {
            timestamp: timestamp.to_owned(),
        }
    })?;

    let row = match query.target.kind {
        SingularTargetKind::Container => snapshot
            .containers
            .into_iter()
            .find(|container| {
                container.id == query.target.value || container.name == query.target.value
            })
            .map(|container| container_snapshot_row(timestamp, container)),
        SingularTargetKind::Image => snapshot
            .images
            .into_iter()
            .find(|image| {
                image.id == query.target.value
                    || image.repository == query.target.value
                    || format!("{}:{}", image.repository, image.tag) == query.target.value
            })
            .map(|image| image_snapshot_row(timestamp, image)),
        SingularTargetKind::Network => snapshot
            .networks
            .into_iter()
            .find(|network| network.id == query.target.value || network.name == query.target.value)
            .map(|network| network_snapshot_row(timestamp, network)),
        SingularTargetKind::Volume => snapshot
            .volumes
            .into_iter()
            .find(|volume| volume.name == query.target.value)
            .map(|volume| volume_snapshot_row(timestamp, volume)),
    }
    .ok_or_else(|| TelemetryError::TargetNotFound {
        target: query.target.value.clone(),
    })?;

    Ok(ExecutionResult { rows: vec![row] })
}

pub fn historical_events<S>(
    query: &EventsQuery,
    store: &S,
) -> Result<ExecutionResult, TelemetryError>
where
    S: TelemetryStore + ?Sized,
{
    let events = match &query.time {
        Some(TimeSelector::Range { from, to }) => store.events_between(from, to)?,
        Some(TimeSelector::Last(duration)) => {
            let now = Utc::now();
            let seconds = crate::alerts::duration_to_std(*duration).as_secs() as i64;
            let from_time = now - chrono::Duration::seconds(seconds);
            let from_str = from_time.to_rfc3339();
            let to_str = now.to_rfc3339();
            store.events_between(&from_str, &to_str)?
        }
        None => return Err(TelemetryError::MissingStore),
    };
    let source = MockEventSource { events };
    events::collect_events(query, &source).map_err(Into::into)
}

fn container_snapshot_row(timestamp: &str, container: Container) -> Row {
    Row {
        fields: BTreeMap::from([
            ("snapshot_at".to_owned(), json_string(timestamp)),
            ("kind".to_owned(), json_string("container")),
            ("id".to_owned(), json_string(container.id)),
            ("name".to_owned(), json_string(container.name)),
            ("image".to_owned(), json_string(container.image)),
            ("status".to_owned(), json_string(container.status)),
            ("state".to_owned(), json_string(container.state)),
            (
                "restart_count".to_owned(),
                container
                    .restart_count
                    .map(json_u64)
                    .unwrap_or(JsonValue::Null),
            ),
        ]),
    }
}

fn image_snapshot_row(timestamp: &str, image: Image) -> Row {
    Row {
        fields: BTreeMap::from([
            ("snapshot_at".to_owned(), json_string(timestamp)),
            ("kind".to_owned(), json_string("image")),
            ("id".to_owned(), json_string(image.id)),
            ("repository".to_owned(), json_string(image.repository)),
            ("tag".to_owned(), json_string(image.tag)),
            ("size".to_owned(), json_string(image.size)),
        ]),
    }
}

fn network_snapshot_row(timestamp: &str, network: Network) -> Row {
    Row {
        fields: BTreeMap::from([
            ("snapshot_at".to_owned(), json_string(timestamp)),
            ("kind".to_owned(), json_string("network")),
            ("id".to_owned(), json_string(network.id)),
            ("name".to_owned(), json_string(network.name)),
            ("driver".to_owned(), json_string(network.driver)),
            ("scope".to_owned(), json_string(network.scope)),
        ]),
    }
}

fn volume_snapshot_row(timestamp: &str, volume: Volume) -> Row {
    Row {
        fields: BTreeMap::from([
            ("snapshot_at".to_owned(), json_string(timestamp)),
            ("kind".to_owned(), json_string("volume")),
            ("name".to_owned(), json_string(volume.name)),
            ("driver".to_owned(), json_string(volume.driver)),
            (
                "mountpoint".to_owned(),
                volume
                    .mountpoint
                    .map(JsonValue::String)
                    .unwrap_or(JsonValue::Null),
            ),
        ]),
    }
}

fn json_string(value: impl Into<String>) -> JsonValue {
    JsonValue::String(value.into())
}

fn json_u64(value: u64) -> JsonValue {
    JsonValue::Number(Number::from(value))
}

#[cfg(test)]
mod tests {
    use crate::{ast::Query, parser};

    use super::*;

    #[test]
    fn stores_and_reads_events_by_range() {
        let mut store = InMemoryTelemetryStore::default();
        store
            .write_event(event("2026-01-01T12:00:00Z", "start"))
            .unwrap();
        store
            .write_event(event("2026-01-01T12:05:00Z", "die"))
            .unwrap();
        store
            .write_event(event("2026-01-01T13:00:00Z", "restart"))
            .unwrap();

        let events = store
            .events_between("2026-01-01T12:00:00Z", "2026-01-01T12:59:59Z")
            .unwrap();

        assert_eq!(events.len(), 2);
    }

    #[test]
    fn inspects_container_at_historical_snapshot() {
        let mut store = InMemoryTelemetryStore::default();
        store
            .write_snapshot(snapshot("2026-01-01 11:59:00", "old-image"))
            .unwrap();
        store
            .write_snapshot(snapshot("2026-01-01 12:00:00", "new-image"))
            .unwrap();
        let Query::Inspect(query) =
            parser::parse("inspect container api at \"2026-01-01 12:00:30\"")
                .unwrap()
                .query
        else {
            panic!("expected inspect query");
        };

        let result = inspect_at(&query, &store).unwrap();

        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["image"],
            JsonValue::String("new-image".to_owned())
        );
        assert_eq!(
            result.rows[0].fields["snapshot_at"],
            JsonValue::String("2026-01-01 12:00:30".to_owned())
        );
    }

    #[test]
    fn replays_historical_events_through_event_pipeline() {
        let mut store = InMemoryTelemetryStore::default();
        store
            .write_event(event("2026-01-01T12:00:00Z", "start"))
            .unwrap();
        store
            .write_event(event("2026-01-01T12:05:00Z", "die"))
            .unwrap();
        let Query::Events(query) = parser::parse(
            "events containers from \"2026-01-01T12:00:00Z\" to \"2026-01-01T12:10:00Z\" where action = \"die\" | select time, action",
        )
        .unwrap()
        .query
        else {
            panic!("expected events query");
        };

        let result = historical_events(&query, &store).unwrap();

        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["action"],
            JsonValue::String("die".to_owned())
        );
    }

    fn snapshot(timestamp: &str, image: &str) -> TelemetrySnapshot {
        TelemetrySnapshot {
            timestamp: timestamp.to_owned(),
            containers: vec![Container {
                id: "abc".to_owned(),
                name: "api".to_owned(),
                image: image.to_owned(),
                status: "Up".to_owned(),
                state: "running".to_owned(),
                ports: Vec::new(),
                labels: Vec::new(),
                created_at: None,
                started_at: None,
                finished_at: None,
                restart_count: Some(1),
            }],
            images: Vec::new(),
            networks: Vec::new(),
            volumes: Vec::new(),
        }
    }

    fn event(time: &str, action: &str) -> DockerEvent {
        DockerEvent {
            time: time.to_owned(),
            event_type: "container".to_owned(),
            action: action.to_owned(),
            actor_id: "abc".to_owned(),
            container: Some("api".to_owned()),
            image: Some("api:latest".to_owned()),
            attributes: vec![("name".to_owned(), "api".to_owned())],
        }
    }
}
