use std::path::Path;

use rusqlite::{Connection, params};
use serde_json;

use crate::{
    docker::{DockerEvent, MetricSample},
    storage::{TelemetryError, TelemetrySnapshot, TelemetryStore},
};

/// Retention policy controlling how long historical data is kept.
#[derive(Debug, Clone)]
pub struct RetentionPolicy {
    /// Maximum age of metric samples in seconds.
    pub metric_max_age_secs: u64,
    /// Maximum age of events in seconds.
    pub event_max_age_secs: u64,
    /// Maximum age of snapshots in seconds.
    pub snapshot_max_age_secs: u64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            metric_max_age_secs: 7 * 24 * 3600,   // 7 days
            event_max_age_secs: 30 * 24 * 3600,    // 30 days
            snapshot_max_age_secs: 30 * 24 * 3600,  // 30 days
        }
    }
}

/// A `TelemetryStore` backed by an embedded SQLite database.
///
/// All data (metrics, events, snapshots) is persisted to a single SQLite file
/// and can be queried by timestamp range. A configurable `RetentionPolicy`
/// controls automatic purging of old data.
pub struct SqliteTelemetryStore {
    conn: Connection,
    retention: RetentionPolicy,
}

impl SqliteTelemetryStore {
    /// Open (or create) a SQLite telemetry store at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TelemetryError> {
        Self::open_with_retention(path, RetentionPolicy::default())
    }

    /// Open with a custom retention policy.
    pub fn open_with_retention(
        path: impl AsRef<Path>,
        retention: RetentionPolicy,
    ) -> Result<Self, TelemetryError> {
        let conn = Connection::open(path).map_err(|e| TelemetryError::Storage(e.to_string()))?;
        let store = Self { conn, retention };
        store.init_schema()?;
        Ok(store)
    }

    /// Create an in-memory SQLite store (useful for testing).
    pub fn in_memory() -> Result<Self, TelemetryError> {
        let conn =
            Connection::open_in_memory().map_err(|e| TelemetryError::Storage(e.to_string()))?;
        let store = Self {
            conn,
            retention: RetentionPolicy::default(),
        };
        store.init_schema()?;
        Ok(store)
    }

    /// Set the retention policy.
    pub fn set_retention(&mut self, retention: RetentionPolicy) {
        self.retention = retention;
    }

    /// Apply the retention policy, deleting records older than the configured thresholds.
    pub fn apply_retention(&mut self) -> Result<RetentionStats, TelemetryError> {
        let now = chrono::Utc::now();
        let metric_cutoff = now
            - chrono::Duration::seconds(self.retention.metric_max_age_secs as i64);
        let event_cutoff =
            now - chrono::Duration::seconds(self.retention.event_max_age_secs as i64);
        let snapshot_cutoff =
            now - chrono::Duration::seconds(self.retention.snapshot_max_age_secs as i64);

        let metric_cutoff_str = metric_cutoff.to_rfc3339();
        let event_cutoff_str = event_cutoff.to_rfc3339();
        let snapshot_cutoff_str = snapshot_cutoff.to_rfc3339();

        let metrics_deleted = self
            .conn
            .execute("DELETE FROM metrics WHERE timestamp < ?1", params![metric_cutoff_str])
            .map_err(|e| TelemetryError::Storage(e.to_string()))?;

        let events_deleted = self
            .conn
            .execute("DELETE FROM events WHERE time < ?1", params![event_cutoff_str])
            .map_err(|e| TelemetryError::Storage(e.to_string()))?;

        let snapshots_deleted = self
            .conn
            .execute(
                "DELETE FROM snapshots WHERE timestamp < ?1",
                params![snapshot_cutoff_str],
            )
            .map_err(|e| TelemetryError::Storage(e.to_string()))?;

        Ok(RetentionStats {
            metrics_deleted,
            events_deleted,
            snapshots_deleted,
        })
    }

    /// Return database statistics.
    pub fn stats(&self) -> Result<StoreStats, TelemetryError> {
        let metric_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM metrics", [], |row| row.get(0))
            .map_err(|e| TelemetryError::Storage(e.to_string()))?;
        let event_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .map_err(|e| TelemetryError::Storage(e.to_string()))?;
        let snapshot_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM snapshots", [], |row| row.get(0))
            .map_err(|e| TelemetryError::Storage(e.to_string()))?;

        Ok(StoreStats {
            metric_count: metric_count as u64,
            event_count: event_count as u64,
            snapshot_count: snapshot_count as u64,
        })
    }

    fn init_schema(&self) -> Result<(), TelemetryError> {
        self.conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS metrics (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                container_id    TEXT NOT NULL,
                container_name  TEXT NOT NULL,
                timestamp       TEXT NOT NULL,
                cpu_percent     REAL,
                memory_usage    INTEGER,
                memory_limit    INTEGER,
                network_rx      INTEGER,
                network_tx      INTEGER,
                disk_read       INTEGER,
                disk_write      INTEGER
            );

            CREATE INDEX IF NOT EXISTS idx_metrics_timestamp ON metrics(timestamp);
            CREATE INDEX IF NOT EXISTS idx_metrics_container ON metrics(container_id);

            CREATE TABLE IF NOT EXISTS events (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                time        TEXT NOT NULL,
                event_type  TEXT NOT NULL,
                action      TEXT NOT NULL,
                actor_id    TEXT NOT NULL,
                container   TEXT,
                image       TEXT,
                attributes  TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_events_time ON events(time);
            CREATE INDEX IF NOT EXISTS idx_events_action ON events(action);

            CREATE TABLE IF NOT EXISTS snapshots (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp   TEXT NOT NULL,
                data        TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_snapshots_timestamp ON snapshots(timestamp);

            CREATE TABLE IF NOT EXISTS alert_history (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp       TEXT NOT NULL,
                container_id    TEXT NOT NULL,
                container_name  TEXT NOT NULL,
                rule_condition  TEXT NOT NULL,
                action_type     TEXT NOT NULL,
                action_detail   TEXT NOT NULL,
                success         INTEGER NOT NULL DEFAULT 1
            );

            CREATE INDEX IF NOT EXISTS idx_alert_history_time ON alert_history(timestamp);
            CREATE INDEX IF NOT EXISTS idx_alert_history_container ON alert_history(container_id);
            ",
            )
            .map_err(|e| TelemetryError::Storage(e.to_string()))?;
        Ok(())
    }
}

impl TelemetryStore for SqliteTelemetryStore {
    fn write_metric(&mut self, sample: MetricSample) -> Result<(), TelemetryError> {
        self.conn
            .execute(
                "INSERT INTO metrics (container_id, container_name, timestamp, cpu_percent, memory_usage, memory_limit, network_rx, network_tx, disk_read, disk_write)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    sample.container_id,
                    sample.container_name,
                    sample.timestamp,
                    sample.cpu_percent,
                    sample.memory_usage_bytes.map(|v| v as i64),
                    sample.memory_limit_bytes.map(|v| v as i64),
                    sample.network_rx_bytes.map(|v| v as i64),
                    sample.network_tx_bytes.map(|v| v as i64),
                    sample.disk_read_bytes.map(|v| v as i64),
                    sample.disk_write_bytes.map(|v| v as i64),
                ],
            )
            .map_err(|e| TelemetryError::Storage(e.to_string()))?;
        Ok(())
    }

    fn latest_metrics(&self) -> Result<Vec<MetricSample>, TelemetryError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT m.container_id, m.container_name, m.timestamp,
                        m.cpu_percent, m.memory_usage, m.memory_limit,
                        m.network_rx, m.network_tx, m.disk_read, m.disk_write
                 FROM metrics m
                 INNER JOIN (
                     SELECT container_id, MAX(timestamp) as max_ts
                     FROM metrics
                     GROUP BY container_id
                 ) latest ON m.container_id = latest.container_id AND m.timestamp = latest.max_ts",
            )
            .map_err(|e| TelemetryError::Storage(e.to_string()))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(MetricSample {
                    container_id: row.get(0)?,
                    container_name: row.get(1)?,
                    timestamp: row.get(2)?,
                    cpu_percent: row.get(3)?,
                    memory_usage_bytes: row.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                    memory_limit_bytes: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
                    network_rx_bytes: row.get::<_, Option<i64>>(6)?.map(|v| v as u64),
                    network_tx_bytes: row.get::<_, Option<i64>>(7)?.map(|v| v as u64),
                    disk_read_bytes: row.get::<_, Option<i64>>(8)?.map(|v| v as u64),
                    disk_write_bytes: row.get::<_, Option<i64>>(9)?.map(|v| v as u64),
                })
            })
            .map_err(|e| TelemetryError::Storage(e.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| TelemetryError::Storage(e.to_string()))
    }

    fn metrics_between(&self, from: &str, to: &str) -> Result<Vec<MetricSample>, TelemetryError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT container_id, container_name, timestamp,
                        cpu_percent, memory_usage, memory_limit,
                        network_rx, network_tx, disk_read, disk_write
                 FROM metrics
                 WHERE timestamp >= ?1 AND timestamp <= ?2
                 ORDER BY timestamp ASC",
            )
            .map_err(|e| TelemetryError::Storage(e.to_string()))?;

        let rows = stmt
            .query_map(params![from, to], |row| {
                Ok(MetricSample {
                    container_id: row.get(0)?,
                    container_name: row.get(1)?,
                    timestamp: row.get(2)?,
                    cpu_percent: row.get(3)?,
                    memory_usage_bytes: row.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                    memory_limit_bytes: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
                    network_rx_bytes: row.get::<_, Option<i64>>(6)?.map(|v| v as u64),
                    network_tx_bytes: row.get::<_, Option<i64>>(7)?.map(|v| v as u64),
                    disk_read_bytes: row.get::<_, Option<i64>>(8)?.map(|v| v as u64),
                    disk_write_bytes: row.get::<_, Option<i64>>(9)?.map(|v| v as u64),
                })
            })
            .map_err(|e| TelemetryError::Storage(e.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| TelemetryError::Storage(e.to_string()))
    }

    fn write_event(&mut self, event: DockerEvent) -> Result<(), TelemetryError> {
        let attributes_json =
            serde_json::to_string(&event.attributes).unwrap_or_else(|_| "[]".to_owned());

        self.conn
            .execute(
                "INSERT INTO events (time, event_type, action, actor_id, container, image, attributes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    event.time,
                    event.event_type,
                    event.action,
                    event.actor_id,
                    event.container,
                    event.image,
                    attributes_json,
                ],
            )
            .map_err(|e| TelemetryError::Storage(e.to_string()))?;
        Ok(())
    }

    fn events_between(&self, from: &str, to: &str) -> Result<Vec<DockerEvent>, TelemetryError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT time, event_type, action, actor_id, container, image, attributes
                 FROM events
                 WHERE time >= ?1 AND time <= ?2
                 ORDER BY time ASC",
            )
            .map_err(|e| TelemetryError::Storage(e.to_string()))?;

        let rows = stmt
            .query_map(params![from, to], |row| {
                let attributes_json: String = row.get(6)?;
                let attributes: Vec<(String, String)> =
                    serde_json::from_str(&attributes_json).unwrap_or_default();
                Ok(DockerEvent {
                    time: row.get(0)?,
                    event_type: row.get(1)?,
                    action: row.get(2)?,
                    actor_id: row.get(3)?,
                    container: row.get(4)?,
                    image: row.get(5)?,
                    attributes,
                })
            })
            .map_err(|e| TelemetryError::Storage(e.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| TelemetryError::Storage(e.to_string()))
    }

    fn write_snapshot(&mut self, snapshot: TelemetrySnapshot) -> Result<(), TelemetryError> {
        let data =
            serde_json::to_string(&snapshot).map_err(|e| TelemetryError::Storage(e.to_string()))?;

        self.conn
            .execute(
                "INSERT INTO snapshots (timestamp, data) VALUES (?1, ?2)",
                params![snapshot.timestamp, data],
            )
            .map_err(|e| TelemetryError::Storage(e.to_string()))?;
        Ok(())
    }

    fn snapshot_at_or_before(
        &self,
        timestamp: &str,
    ) -> Result<Option<TelemetrySnapshot>, TelemetryError> {
        let result: Option<String> = self
            .conn
            .query_row(
                "SELECT data FROM snapshots WHERE timestamp <= ?1 ORDER BY timestamp DESC LIMIT 1",
                params![timestamp],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| TelemetryError::Storage(e.to_string()))?;

        match result {
            Some(data) => {
                let snapshot: TelemetrySnapshot = serde_json::from_str(&data)
                    .map_err(|e| TelemetryError::Storage(e.to_string()))?;
                Ok(Some(snapshot))
            }
            None => Ok(None),
        }
    }

    fn write_alert_event(&mut self, event: crate::storage::AlertHistoryEvent) -> Result<(), TelemetryError> {
        self.conn
            .execute(
                "INSERT INTO alert_history (timestamp, container_id, container_name, rule_condition, action_type, action_detail, success)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    event.timestamp,
                    event.container_id,
                    event.container_name,
                    event.rule_condition,
                    event.action_type,
                    event.action_detail,
                    event.success as i32,
                ],
            )
            .map_err(|e| TelemetryError::Storage(e.to_string()))?;
        Ok(())
    }

    fn alert_history(
        &self,
        from: &str,
        to: &str,
    ) -> Result<Vec<crate::storage::AlertHistoryEvent>, TelemetryError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT timestamp, container_id, container_name, rule_condition, action_type, action_detail, success
                 FROM alert_history
                 WHERE timestamp >= ?1 AND timestamp <= ?2
                 ORDER BY timestamp DESC",
            )
            .map_err(|e| TelemetryError::Storage(e.to_string()))?;

        let rows = stmt
            .query_map(params![from, to], |row| {
                let success_int: i32 = row.get(6)?;
                Ok(crate::storage::AlertHistoryEvent {
                    timestamp: row.get(0)?,
                    container_id: row.get(1)?,
                    container_name: row.get(2)?,
                    rule_condition: row.get(3)?,
                    action_type: row.get(4)?,
                    action_detail: row.get(5)?,
                    success: success_int != 0,
                })
            })
            .map_err(|e| TelemetryError::Storage(e.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| TelemetryError::Storage(e.to_string()))
    }
}

/// Statistics about data purged by retention policy.
#[derive(Debug, Clone)]
pub struct RetentionStats {
    pub metrics_deleted: usize,
    pub events_deleted: usize,
    pub snapshots_deleted: usize,
}

/// Statistics about the store's contents.
#[derive(Debug, Clone)]
pub struct StoreStats {
    pub metric_count: u64,
    pub event_count: u64,
    pub snapshot_count: u64,
}

/// Trait extension for optional query results from rusqlite.
trait OptionalExt<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalExt<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::Container;

    #[test]
    fn sqlite_stores_and_reads_metrics() {
        let mut store = SqliteTelemetryStore::in_memory().unwrap();

        store.write_metric(sample("abc", "api", "2026-01-01T12:00:00Z", 85.0)).unwrap();
        store.write_metric(sample("abc", "api", "2026-01-01T12:01:00Z", 90.0)).unwrap();
        store.write_metric(sample("def", "worker", "2026-01-01T12:00:00Z", 20.0)).unwrap();

        let latest = store.latest_metrics().unwrap();
        assert_eq!(latest.len(), 2);

        let between = store.metrics_between("2026-01-01T12:00:00Z", "2026-01-01T12:00:30Z").unwrap();
        assert_eq!(between.len(), 2); // api@12:00 and worker@12:00
    }

    #[test]
    fn sqlite_stores_and_reads_events() {
        let mut store = SqliteTelemetryStore::in_memory().unwrap();

        store.write_event(event("2026-01-01T12:00:00Z", "start")).unwrap();
        store.write_event(event("2026-01-01T12:05:00Z", "die")).unwrap();
        store.write_event(event("2026-01-01T13:00:00Z", "restart")).unwrap();

        let events = store.events_between("2026-01-01T12:00:00Z", "2026-01-01T12:59:59Z").unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].action, "start");
        assert_eq!(events[1].action, "die");
    }

    #[test]
    fn sqlite_stores_and_reads_snapshots() {
        let mut store = SqliteTelemetryStore::in_memory().unwrap();

        store.write_snapshot(snapshot("2026-01-01 11:59:00", "old-image")).unwrap();
        store.write_snapshot(snapshot("2026-01-01 12:00:00", "new-image")).unwrap();

        // Query at exact match
        let snap = store.snapshot_at_or_before("2026-01-01 12:00:00").unwrap().unwrap();
        assert_eq!(snap.containers[0].image, "new-image");

        // Query after both → gets latest
        let snap = store.snapshot_at_or_before("2026-01-01 12:00:30").unwrap().unwrap();
        assert_eq!(snap.containers[0].image, "new-image");

        // Query between → gets earlier
        let snap = store.snapshot_at_or_before("2026-01-01 11:59:30").unwrap().unwrap();
        assert_eq!(snap.containers[0].image, "old-image");

        // Query before all → none
        let snap = store.snapshot_at_or_before("2026-01-01 11:00:00").unwrap();
        assert!(snap.is_none());
    }

    #[test]
    fn sqlite_inspect_at_works() {
        let mut store = SqliteTelemetryStore::in_memory().unwrap();

        store.write_snapshot(snapshot("2026-01-01 12:00:00", "api:v2")).unwrap();

        let query = crate::ast::InspectQuery {
            target: crate::ast::SingularTarget {
                kind: crate::ast::SingularTargetKind::Container,
                value: "api".to_owned(),
            },
            at: Some("2026-01-01 12:00:30".to_owned()),
        };

        let result = crate::storage::inspect_at(&query, &store).unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["image"],
            serde_json::Value::String("api:v2".to_owned())
        );
    }

    #[test]
    fn sqlite_historical_events_works() {
        let mut store = SqliteTelemetryStore::in_memory().unwrap();

        store.write_event(event("2026-01-01T12:00:00Z", "start")).unwrap();
        store.write_event(event("2026-01-01T12:05:00Z", "die")).unwrap();

        let query = crate::ast::EventsQuery {
            target: crate::ast::CollectionTarget::Containers,
            time: Some(crate::ast::TimeSelector::Range {
                from: "2026-01-01T12:00:00Z".to_owned(),
                to: "2026-01-01T12:10:00Z".to_owned(),
            }),
            filter: Some(crate::ast::Expression::Comparison {
                field: "action".to_owned(),
                operator: crate::ast::Operator::Eq,
                value: crate::ast::Value::String("die".to_owned()),
            }),
            pipeline: vec![],
        };

        let result = crate::storage::historical_events(&query, &store).unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["action"],
            serde_json::Value::String("die".to_owned())
        );
    }

    #[test]
    fn sqlite_retention_deletes_old_data() {
        let mut store = SqliteTelemetryStore::in_memory().unwrap();
        store.set_retention(RetentionPolicy {
            metric_max_age_secs: 0, // delete everything
            event_max_age_secs: 0,
            snapshot_max_age_secs: 0,
        });

        store.write_metric(sample("abc", "api", "2020-01-01T00:00:00Z", 50.0)).unwrap();
        store.write_event(event("2020-01-01T00:00:00Z", "start")).unwrap();
        store.write_snapshot(snapshot("2020-01-01 00:00:00", "old")).unwrap();

        let stats_before = store.stats().unwrap();
        assert_eq!(stats_before.metric_count, 1);
        assert_eq!(stats_before.event_count, 1);
        assert_eq!(stats_before.snapshot_count, 1);

        let retention_stats = store.apply_retention().unwrap();
        assert_eq!(retention_stats.metrics_deleted, 1);
        assert_eq!(retention_stats.events_deleted, 1);
        assert_eq!(retention_stats.snapshots_deleted, 1);

        let stats_after = store.stats().unwrap();
        assert_eq!(stats_after.metric_count, 0);
        assert_eq!(stats_after.event_count, 0);
        assert_eq!(stats_after.snapshot_count, 0);
    }

    #[test]
    fn sqlite_store_stats() {
        let mut store = SqliteTelemetryStore::in_memory().unwrap();

        store.write_metric(sample("abc", "api", "2026-01-01T12:00:00Z", 50.0)).unwrap();
        store.write_metric(sample("def", "worker", "2026-01-01T12:00:00Z", 30.0)).unwrap();
        store.write_event(event("2026-01-01T12:00:00Z", "start")).unwrap();

        let stats = store.stats().unwrap();
        assert_eq!(stats.metric_count, 2);
        assert_eq!(stats.event_count, 1);
        assert_eq!(stats.snapshot_count, 0);
    }

    #[test]
    fn sqlite_preserves_event_attributes() {
        let mut store = SqliteTelemetryStore::in_memory().unwrap();

        let ev = DockerEvent {
            time: "2026-01-01T12:00:00Z".to_owned(),
            event_type: "container".to_owned(),
            action: "start".to_owned(),
            actor_id: "abc".to_owned(),
            container: Some("api".to_owned()),
            image: Some("api:latest".to_owned()),
            attributes: vec![
                ("name".to_owned(), "api".to_owned()),
                ("signal".to_owned(), "15".to_owned()),
            ],
        };
        store.write_event(ev).unwrap();

        let events = store.events_between("2026-01-01T00:00:00Z", "2026-01-01T23:59:59Z").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].attributes.len(), 2);
        assert_eq!(events[0].attributes[0], ("name".to_owned(), "api".to_owned()));
    }

    fn sample(id: &str, name: &str, ts: &str, cpu: f64) -> MetricSample {
        MetricSample {
            container_id: id.to_owned(),
            container_name: name.to_owned(),
            timestamp: ts.to_owned(),
            cpu_percent: Some(cpu),
            memory_usage_bytes: Some(128),
            memory_limit_bytes: Some(1024),
            network_rx_bytes: Some(10),
            network_tx_bytes: Some(20),
            disk_read_bytes: Some(30),
            disk_write_bytes: Some(40),
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
}
