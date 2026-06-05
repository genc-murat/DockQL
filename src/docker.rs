//! Docker client abstraction and domain types.
//!
//! Defines the [`DockerClient`] async trait and the [`BollardDockerClient`]
//! implementation. Also provides domain types ([`Container`], [`Image`],
//! [`Network`], [`Volume`], [`MetricSample`], [`DockerEvent`]) and the
//! [`MockDockerClient`] for unit testing.
//!
//! # Example
//!
//! ```ignore
//! let docker = BollardDockerClient::connect_with_config(&config)?;
//! let containers = docker.list_containers().await?;
//! ```

use std::collections::HashMap;
use std::time::Duration;

use crate::ONE_GB;
use crate::config::DolConfig;
use bollard::container::LogOutput;
use bollard::models::{
    ContainerInspectResponse, ContainerSummary, EventMessage, ImageSummary,
    PortSummary, Volume as BollardVolume,
};
use bollard::models::Network as BollardNetwork;
use bollard::query_parameters as qp;
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use thiserror::Error;
use tokio::time::timeout;

/// Shared type alias for the complex Docker event stream return type.
pub type DockerEventStream = Pin<Box<dyn Stream<Item = Result<DockerEvent, DockerError>> + Send>>;

// ── Domain types ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub status: String,
    pub state: String,
    pub ports: Vec<String>,
    pub labels: Vec<String>,
    pub created_at: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub restart_count: Option<u64>,
    pub health: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Image {
    pub id: String,
    pub repository: String,
    pub tag: String,
    pub digest: Option<String>,
    pub size: String,
    pub created_at: Option<String>,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Network {
    pub id: String,
    pub name: String,
    pub driver: String,
    pub scope: String,
    pub containers: Vec<String>,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Volume {
    pub name: String,
    pub driver: String,
    pub mountpoint: Option<String>,
    pub scope: Option<String>,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricSample {
    pub container_id: String,
    pub container_name: String,
    pub timestamp: String,
    pub cpu_percent: Option<f64>,
    pub memory_usage_bytes: Option<u64>,
    pub memory_limit_bytes: Option<u64>,
    pub network_rx_bytes: Option<u64>,
    pub network_tx_bytes: Option<u64>,
    pub disk_read_bytes: Option<u64>,
    pub disk_write_bytes: Option<u64>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct DockerEvent {
    pub time: String,
    pub event_type: String,
    pub action: String,
    pub actor_id: String,
    pub container: Option<String>,
    pub image: Option<String>,
    pub attributes: Vec<(String, String)>,
}

// ── Error type ─────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum DockerError {
    #[error("bollard error: {0}")]
    Bollard(#[from] bollard::errors::Error),
    #[error("container not found: {0}")]
    NotFound(String),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("Docker API call timed out after {0}s")]
    Timeout(u64),
}

// ── Async trait ────────────────────────────────────────────────────────────

pub trait DockerClient {
    fn list_containers(&self) -> impl std::future::Future<Output = Result<Vec<Container>, DockerError>> + Send;
    fn list_images(&self) -> impl std::future::Future<Output = Result<Vec<Image>, DockerError>> + Send;
    fn list_networks(&self) -> impl std::future::Future<Output = Result<Vec<Network>, DockerError>> + Send;
    fn list_volumes(&self) -> impl std::future::Future<Output = Result<Vec<Volume>, DockerError>> + Send;
    fn inspect_container(&self, id: &str) -> impl std::future::Future<Output = Result<Container, DockerError>> + Send;
    fn container_logs(&self, id: &str, tail: usize) -> impl std::future::Future<Output = Result<Vec<String>, DockerError>> + Send;
    fn container_stats(&self, id: &str) -> impl std::future::Future<Output = Result<MetricSample, DockerError>> + Send;
    fn events_stream(&self, since: Option<&str>, until: Option<&str>) -> impl std::future::Future<Output = Result<DockerEventStream, DockerError>> + Send;
    fn ping(&self) -> impl std::future::Future<Output = Result<bool, DockerError>> + Send;
}

// ── Docker API timeout configuration ───────────────────────────────────────

/// Holds all configurable timeout values for Docker API calls.
#[derive(Debug, Clone)]
pub struct DockerApiConfig {
    pub call_timeout: Duration,
    pub quick_timeout: Duration,
    pub max_retries: u32,
    pub retry_base_ms: u64,
}

impl Default for DockerApiConfig {
    fn default() -> Self {
        Self {
            call_timeout: Duration::from_secs(30),
            quick_timeout: Duration::from_secs(10),
            max_retries: 2,
            retry_base_ms: 200,
        }
    }
}

impl From<&DolConfig> for DockerApiConfig {
    fn from(cfg: &DolConfig) -> Self {
        Self {
            call_timeout: Duration::from_secs(cfg.api_timeout.unwrap_or(30)),
            quick_timeout: Duration::from_secs(cfg.api_quick_timeout.unwrap_or(10)),
            max_retries: 2,
            retry_base_ms: 200,
        }
    }
}

// ── Timeout & retry helpers ────────────────────────────────────────────────

/// Run a bollard API future with a standard timeout.
/// Returns `DockerError::Timeout` if the operation does not complete in time.
async fn docker_call_timeout<T>(
    fut: impl std::future::Future<Output = Result<T, bollard::errors::Error>>,
    duration: Duration,
) -> Result<T, DockerError> {
    timeout(duration, fut)
        .await
        .map_err(|_| DockerError::Timeout(duration.as_secs()))?
        .map_err(DockerError::Bollard)
}

/// Run a bollard API future with the default timeout from config.
async fn docker_call<T>(
    cfg: &DockerApiConfig,
    fut: impl std::future::Future<Output = Result<T, bollard::errors::Error>>,
) -> Result<T, DockerError> {
    docker_call_timeout(fut, cfg.call_timeout).await
}

/// Run a bollard API future with the quick (short) timeout from config.
async fn docker_call_quick<T>(
    cfg: &DockerApiConfig,
    fut: impl std::future::Future<Output = Result<T, bollard::errors::Error>>,
) -> Result<T, DockerError> {
    docker_call_timeout(fut, cfg.quick_timeout).await
}

/// Run a bollard API future factory with retries (exponential backoff) for
/// transient errors. Uses a closure so a fresh future is created on each retry
/// attempt. Only operations that are safe to retry (read-only queries like
/// `inspect_container`, `container_stats`) should use this.
async fn docker_call_with_retry<T, F, Fut>(
    cfg: &DockerApiConfig,
    f: F,
) -> Result<T, DockerError>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, bollard::errors::Error>>,
{
    let mut last_err = None;
    for attempt in 0..=cfg.max_retries {
        match timeout(cfg.call_timeout, f()).await {
            Ok(Ok(val)) => return Ok(val),
            Ok(Err(e)) => {
                last_err = Some(DockerError::Bollard(e));
                if attempt < cfg.max_retries {
                    let delay = cfg.retry_base_ms * (1u64 << attempt);
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
            }
            Err(_) => {
                last_err = Some(DockerError::Timeout(cfg.call_timeout.as_secs()));
                if attempt < cfg.max_retries {
                    let delay = cfg.retry_base_ms * (1u64 << attempt);
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
            }
        }
    }
    Err(last_err.unwrap_or(DockerError::Timeout(cfg.call_timeout.as_secs())))
}

pub async fn list_running_containers<C: DockerClient>(client: &C) -> Result<Vec<Container>, DockerError> {
    Ok(client
        .list_containers()
        .await?
        .into_iter()
        .filter(|c| c.state == "running")
        .collect())
}

// ── BollardDockerClient ───────────────────────────────────────────────────

#[derive(Clone)]
pub struct BollardDockerClient {
    docker: bollard::Docker,
    api_cfg: DockerApiConfig,
}

impl BollardDockerClient {
    /// Connect with default timeout values.
    pub fn connect() -> Result<Self, DockerError> {
        Self::connect_with_config(&DolConfig::default())
    }

    /// Connect with timeout values from the given config.
    pub fn connect_with_config(config: &DolConfig) -> Result<Self, DockerError> {
        let docker = bollard::Docker::connect_with_local_defaults()
            .map_err(DockerError::Bollard)?;
        Ok(Self {
            docker,
            api_cfg: DockerApiConfig::from(config),
        })
    }

    /// Connect to a specific Docker host with timeout values from config.
    pub fn connect_with_host(host: &str, config: &DolConfig) -> Result<Self, DockerError> {
        let docker = if host.starts_with("tcp://") || host.starts_with("http://") {
            let h = host
                .strip_prefix("tcp://")
                .or_else(|| host.strip_prefix("http://"))
                .unwrap_or(host);
            bollard::Docker::connect_with_http(h, 120, bollard::API_DEFAULT_VERSION)
                .map_err(DockerError::Bollard)?
        } else if host.starts_with("unix://") {
            let path = host.strip_prefix("unix://").unwrap_or(host);
            bollard::Docker::connect_with_socket(path, 120, bollard::API_DEFAULT_VERSION)
                .map_err(DockerError::Bollard)?
        } else {
            bollard::Docker::connect_with_local_defaults()
                .map_err(DockerError::Bollard)?
        };
        Ok(Self {
            docker,
            api_cfg: DockerApiConfig::from(config),
        })
    }

    /// Get a reference to the API config (used by the metrics collector).
    #[must_use]
    pub const fn api_config(&self) -> &DockerApiConfig {
        &self.api_cfg
    }

    #[must_use]
    pub fn from_docker(docker: bollard::Docker) -> Self {
        Self {
            docker,
            api_cfg: DockerApiConfig::default(),
        }
    }
}

impl DockerClient for BollardDockerClient {
    async fn list_containers(&self) -> Result<Vec<Container>, DockerError> {
        let options = Some(qp::ListContainersOptions {
            all: true,
            ..Default::default()
        });
        let summaries = docker_call(&self.api_cfg, self.docker.list_containers(options)).await?;
        let mut containers: Vec<Container> = summaries.iter().map(container_from_summary).collect();

        if !containers.is_empty() {
            let ids: Vec<String> = containers.iter().map(|c| c.id.clone()).collect();
            for (i, id) in ids.iter().enumerate() {
                if let Ok(inspect) = docker_call_with_retry(
                    &self.api_cfg,
                    || self.docker.inspect_container(id, None),
                )
                .await
                {
                    let s = inspect.state.as_ref();
                    containers[i].started_at = s
                        .and_then(|st| st.started_at.clone())
                        .filter(|ts| !ts.is_empty() && !ts.starts_with("0001"));
                    containers[i].finished_at = s
                        .and_then(|st| st.finished_at.clone())
                        .filter(|ts| !ts.is_empty() && !ts.starts_with("0001"));
                    containers[i].restart_count = inspect
                        .restart_count
                        .map(|c| c.max(0) as u64);
                    containers[i].health = s
                        .and_then(|st| st.health.as_ref()).map(|h| h.status.map(|e| format!("{e:?}")).unwrap_or_default())
                        .filter(|s| !s.is_empty());
                }
            }
        }

        Ok(containers)
    }

    async fn list_images(&self) -> Result<Vec<Image>, DockerError> {
        let summaries = docker_call(
            &self.api_cfg,
            self.docker.list_images(None::<qp::ListImagesOptions>),
        )
        .await?;
        Ok(summaries.iter().map(image_from_summary).collect())
    }

    async fn list_networks(&self) -> Result<Vec<Network>, DockerError> {
        let networks = docker_call(
            &self.api_cfg,
            self.docker.list_networks(None::<qp::ListNetworksOptions>),
        )
        .await?;
        Ok(networks.into_iter().map(network_from_bollard).collect())
    }

    async fn list_volumes(&self) -> Result<Vec<Volume>, DockerError> {
        let response = docker_call(
            &self.api_cfg,
            self.docker.list_volumes(None::<qp::ListVolumesOptions>),
        )
        .await?;
        Ok(response
            .volumes
            .unwrap_or_default()
            .into_iter()
            .map(volume_from_bollard)
            .collect())
    }

    async fn inspect_container(&self, id: &str) -> Result<Container, DockerError> {
        let inspect = docker_call_with_retry(
            &self.api_cfg,
            || self.docker.inspect_container(id, None),
        )
        .await?;
        Ok(container_from_inspect(&inspect))
    }

    async fn container_logs(&self, id: &str, tail: usize) -> Result<Vec<String>, DockerError> {
        let options = Some(qp::LogsOptions {
            tail: tail.to_string(),
            stdout: true,
            stderr: true,
            ..Default::default()
        });
        let stream_fut = self.docker.logs(id, options);
        let mut stream = stream_fut;
        let mut lines = Vec::new();
        loop {
            let next = tokio::time::timeout(self.api_cfg.call_timeout, stream.next()).await;
            match next {
                Ok(Some(Ok(LogOutput::StdOut { message } | LogOutput::StdErr { message }))) => {
                    let s = String::from_utf8_lossy(&message).to_string();
                    for line in s.lines() {
                        lines.push(line.to_owned());
                    }
                }
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(e))) => return Err(DockerError::Bollard(e)),
                Ok(None) => break,
                Err(_) => return Err(DockerError::Timeout(self.api_cfg.call_timeout.as_secs())),
            }
        }
        Ok(lines)
    }

    async fn container_stats(&self, id: &str) -> Result<MetricSample, DockerError> {
        use bollard::query_parameters::StatsOptions;
        let stream_fut = self.docker.stats(id, Some(StatsOptions { stream: false, ..Default::default() }));
        let mut stream = stream_fut;
        let next = tokio::time::timeout(self.api_cfg.call_timeout, stream.next()).await;
        match next {
            Ok(Some(Ok(stats))) => {
                let container_name = docker_call_with_retry(
                    &self.api_cfg,
                    || self.docker.inspect_container(id, None),
                )
                .await
                .ok()
                .and_then(|i| i.name)
                .map_or_else(|| id.to_owned(), |n| n.trim_start_matches('/').to_owned());
                Ok(metric_sample_from_bollard_stats(id, &container_name, &stats))
            }
            Ok(Some(Err(e))) => Err(DockerError::Bollard(e)),
            Ok(None) => Err(DockerError::NotFound(id.to_owned())),
            Err(_) => Err(DockerError::Timeout(self.api_cfg.call_timeout.as_secs())),
        }
    }

    async fn events_stream(&self, since: Option<&str>, until: Option<&str>) -> Result<DockerEventStream, DockerError> {
        let options = Some(qp::EventsOptions {
            since: since.map(std::borrow::ToOwned::to_owned),
            until: until.map(std::borrow::ToOwned::to_owned),
            ..Default::default()
        });
        let stream = self.docker.events(options);
        let mapped = stream.map(|item| match item {
            Ok(msg) => Ok(docker_event_from_message(msg)),
            Err(e) => Err(DockerError::Bollard(e)),
        });
        Ok(Box::pin(mapped))
    }

    async fn ping(&self) -> Result<bool, DockerError> {
        docker_call_quick(&self.api_cfg, self.docker.ping()).await?;
        Ok(true)
    }
}

// ── MockDockerClient (kept for unit tests) ─────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct MockDockerClient {
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
    pub networks: Vec<Network>,
    pub volumes: Vec<Volume>,
    pub logs: HashMap<String, Vec<String>>,
    pub events: Vec<DockerEvent>,
}

impl DockerClient for MockDockerClient {
    async fn list_containers(&self) -> Result<Vec<Container>, DockerError> {
        Ok(self.containers.clone())
    }

    async fn list_images(&self) -> Result<Vec<Image>, DockerError> {
        Ok(self.images.clone())
    }

    async fn list_networks(&self) -> Result<Vec<Network>, DockerError> {
        Ok(self.networks.clone())
    }

    async fn list_volumes(&self) -> Result<Vec<Volume>, DockerError> {
        Ok(self.volumes.clone())
    }

    async fn inspect_container(&self, id: &str) -> Result<Container, DockerError> {
        self.containers
            .iter()
            .find(|c| c.id.starts_with(id) || c.name == id)
            .cloned()
            .ok_or_else(|| DockerError::NotFound(id.to_owned()))
    }

    async fn container_logs(&self, id: &str, _tail: usize) -> Result<Vec<String>, DockerError> {
        let key = self
            .containers
            .iter()
            .find(|c| c.id.starts_with(id) || c.name == id).map_or_else(|| id.to_owned(), |c| c.id.clone());
        Ok(self.logs.get(&key).cloned().unwrap_or_default())
    }

    async fn container_stats(&self, id: &str) -> Result<MetricSample, DockerError> {
        let container = self.inspect_container(id).await?;
        Ok(MetricSample {
            container_id: container.id.clone(),
            container_name: container.name,
            timestamp: chrono::Utc::now().to_rfc3339(),
            cpu_percent: None,
            memory_usage_bytes: None,
            memory_limit_bytes: None,
            network_rx_bytes: None,
            network_tx_bytes: None,
            disk_read_bytes: None,
            disk_write_bytes: None,
        })
    }

    async fn events_stream(&self, _since: Option<&str>, _until: Option<&str>) -> Result<DockerEventStream, DockerError> {
        let events: Vec<Result<DockerEvent, DockerError>> =
            self.events.clone().into_iter().map(Ok).collect();
        Ok(Box::pin(futures_util::stream::iter(events)))
    }

    async fn ping(&self) -> Result<bool, DockerError> {
        Ok(true)
    }
}

// ── Conversion helpers ─────────────────────────────────────────────────────

fn container_from_summary(s: &ContainerSummary) -> Container {
    Container {
        id: s.id.as_deref().unwrap_or_default().to_owned(),
        name: s.names.as_ref().and_then(|n| n.first())
            .map(|n| n.trim_start_matches('/').to_owned()).unwrap_or_default(),
        image: s.image.as_deref().unwrap_or_default().to_owned(),
        status: s.status.as_deref().unwrap_or_default().to_owned(),
        state: s.state.as_ref().map(|e| format!("{e:?}").to_lowercase()).unwrap_or_default(),
        ports: ports_from_summary(s.ports.as_ref()),
        labels: labels_map_to_vec(s.labels.as_ref()),
        created_at: s.created.map(unix_to_iso),
        started_at: None,
        finished_at: None,
        restart_count: None,
        health: None,
    }
}

fn container_from_inspect(inspect: &ContainerInspectResponse) -> Container {
    let config = inspect.config.as_ref();
    let state = inspect.state.as_ref();
    let net_settings = inspect.network_settings.as_ref();

    let state_status = state
        .and_then(|s| s.status.as_ref())
        .map(|e| format!("{e:?}").to_lowercase())
        .unwrap_or_default();

    Container {
        id: inspect.id.as_deref().unwrap_or_default().to_owned(),
        name: inspect.name.as_deref()
            .map(|n| n.trim_start_matches('/').to_owned()).unwrap_or_default(),
        image: config.and_then(|c| c.image.as_deref()).unwrap_or_default().to_owned(),
        status: state_status.clone(),
        state: state_status,
        ports: net_settings
            .and_then(|ns| ns.ports.as_ref())
            .map(|ports_obj| {
                ports_obj.iter()
                    .flat_map(|(container_port, bindings)| {
                        let arr = bindings.as_ref();
                        if arr.is_none_or(std::vec::Vec::is_empty) {
                            vec![container_port.clone()]
                        } else {
                            arr.expect("bindings should be Some here (checked by is_none_or)").iter().map(|b| {
                                let host_ip = b.host_ip.as_deref().unwrap_or("");
                                let host_port = b.host_port.as_deref().unwrap_or("");
                                if host_port.is_empty() { container_port.clone() }
                                else if host_ip.is_empty() || host_ip == "0.0.0.0" {
                                    format!("{host_port}->{container_port}")
                                } else { format!("{host_ip}:{host_port}->{container_port}") }
                            }).collect::<Vec<_>>()
                        }
                    }).collect()
            }).unwrap_or_default(),
        labels: config.and_then(|c| c.labels.as_ref()).map(|l| labels_map_to_vec(Some(l))).unwrap_or_default(),
        created_at: inspect.created.clone().filter(|s| !s.is_empty()),
        started_at: state.and_then(|s| s.started_at.clone()).filter(|s| !s.starts_with("0001")),
        finished_at: state.and_then(|s| s.finished_at.clone()).filter(|s| !s.starts_with("0001")),
        restart_count: inspect.restart_count.map(|c| c.max(0) as u64),
        health: state.and_then(|s| s.health.as_ref())
            .and_then(|h| h.status.as_ref().map(|e| format!("{e:?}")))
            .filter(|s| !s.is_empty()),
    }
}

fn image_from_summary(s: &ImageSummary) -> Image {
    let id = s.id.clone();
    let (repository, tag) = s.repo_tags.first().map_or_else(|| (id.clone(), "latest".to_owned()), |full| if let Some((repo, t)) = full.rsplit_once(':') {
            (repo.to_owned(), t.to_owned())
        } else { (full.clone(), "latest".to_owned()) });
    let repository = if repository == "<none>" { id.clone() } else { repository };

    Image {
        id,
        repository,
        tag,
        digest: s.repo_digests.first().cloned(),
        size: format_bytes_human(s.size.max(0) as u64),
        created_at: Some(unix_to_iso(s.created)),
        labels: Some(&s.labels).map(|l| labels_map_to_vec(Some(l))).unwrap_or_default(),
    }
}

fn network_from_bollard(n: BollardNetwork) -> Network {
    Network {
        id: n.id.unwrap_or_default(),
        name: n.name.unwrap_or_default(),
        driver: n.driver.unwrap_or_default(),
        scope: n.scope.unwrap_or_default(),
        containers: Vec::new(), // not available from list_networks in bollard
        labels: n.labels.as_ref().map(|l| labels_map_to_vec(Some(l))).unwrap_or_default(),
    }
}

fn volume_from_bollard(v: BollardVolume) -> Volume {
    Volume {
        name: v.name,
        driver: v.driver,
        mountpoint: Some(v.mountpoint).filter(|s| !s.is_empty()),
        scope: v.scope.map(|s| format!("{s:?}").to_lowercase()),
        labels: Some(&v.labels).map(|l| labels_map_to_vec(Some(l))).unwrap_or_default(),
    }
}

// ── Event conversion ───────────────────────────────────────────────────────

pub fn docker_event_from_message(msg: EventMessage) -> DockerEvent {
    let actor: Option<&bollard::models::EventActor> = msg.actor.as_ref();
    let attributes: Vec<(String, String)> = actor
        .and_then(|a| a.attributes.as_ref())
        .map(|attrs| attrs.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();

    let container = attributes.iter().find(|(k, _)| k == "name").map(|(_, v)| v.clone())
        .or_else(|| msg.actor.as_ref().and_then(|a| a.id.clone()));
    let image = attributes.iter().find(|(k, _)| k == "image").map(|(_, v)| v.clone());

    DockerEvent {
        time: msg.time.map(unix_to_iso)
            .or_else(|| msg.time_nano.map(|n| unix_to_iso(n / 1_000_000_000)))
            .unwrap_or_default(),
        event_type: msg.typ.map(|e| format!("{e:?}")).unwrap_or_default(),
        action: msg.action.unwrap_or_default(),
        actor_id: actor.and_then(|a| a.id.clone()).unwrap_or_default(),
        container,
        image,
        attributes,
    }
}

// ── Stats/metrics helpers ─────────────────────────────────────────────────

#[allow(clippy::similar_names)]
pub fn metric_sample_from_stats_json(value: &serde_json::Value) -> Result<MetricSample, String> {
    let mem_usage = optional_string(value, &["MemUsage"]);
    let (memory_usage_bytes, memory_limit_bytes) = mem_usage.as_deref()
        .map(parse_usage_pair).transpose()
        .map_err(|()| "invalid MemUsage".to_string())?
        .unwrap_or((None, None));
    let net_io = optional_string(value, &["NetIO"]);
    let (network_rx_bytes, network_tx_bytes) = net_io.as_deref()
        .map(parse_usage_pair).transpose()
        .map_err(|()| "invalid NetIO".to_string())?
        .unwrap_or((None, None));
    let block_io = optional_string(value, &["BlockIO"]);
    let (disk_read_bytes, disk_write_bytes) = block_io.as_deref()
        .map(parse_usage_pair).transpose()
        .map_err(|()| "invalid BlockIO".to_string())?
        .unwrap_or((None, None));

    Ok(MetricSample {
        container_id: string(value, &["ID", "Container"]),
        container_name: string(value, &["Name"]),
        timestamp: { let ts = string(value, &["Timestamp"]); if ts.is_empty() { chrono::Utc::now().to_rfc3339() } else { ts } },
        cpu_percent: optional_string(value, &["CPUPerc"]).as_deref().and_then(|s| s.trim().trim_end_matches('%').parse().ok()),
        memory_usage_bytes, memory_limit_bytes, network_rx_bytes, network_tx_bytes, disk_read_bytes, disk_write_bytes,
    })
}

#[must_use]
pub fn metric_sample_from_bollard_stats(container_id: &str, container_name: &str, stats: &bollard::models::ContainerStatsResponse) -> MetricSample {
    let cpu_percent = stats.cpu_stats.as_ref().and_then(|cpu| {
        let total = cpu.cpu_usage.as_ref().and_then(|u| u.total_usage).unwrap_or(0);
        let system = cpu.system_cpu_usage.unwrap_or(0);
        let cpus = cpu.online_cpus.unwrap_or(1).max(1);
        if system > 0 && total > 0 { Some((total as f64 / system as f64) * f64::from(cpus) * 100.0) } else { None }
    });
    MetricSample {
        container_id: container_id.to_owned(), container_name: container_name.to_owned(),
        timestamp: chrono::Utc::now().to_rfc3339(), cpu_percent,
        memory_usage_bytes: stats.memory_stats.as_ref().and_then(|m| m.usage),
        memory_limit_bytes: stats.memory_stats.as_ref().and_then(|m| m.limit),
        network_rx_bytes: stats.networks.as_ref().and_then(|n| n.values().next()).and_then(|n| n.rx_bytes),
        network_tx_bytes: stats.networks.as_ref().and_then(|n| n.values().next()).and_then(|n| n.tx_bytes),
        disk_read_bytes: None, disk_write_bytes: None,
    }
}

// ── Utility functions ──────────────────────────────────────────────────────

fn labels_map_to_vec(labels: Option<&HashMap<String, String>>) -> Vec<String> {
    labels.map(|m| m.iter().map(|(k, v)| format!("{k}={v}")).collect()).unwrap_or_default()
}

fn ports_from_summary(ports: Option<&Vec<PortSummary>>) -> Vec<String> {
    ports.map(|p| p.iter().map(|port| {
        let typ = port.typ.as_ref().map_or_else(|| "tcp".to_owned(), |e| format!("{e:?}").to_lowercase());
        port.public_port.map_or_else(
            || format!("{}/{}", port.private_port, typ),
            |host_port| format!("{}:{}->{}/{}", port.ip.as_deref().unwrap_or("0.0.0.0"), host_port, port.private_port, typ),
        )
    }).collect()).unwrap_or_default()
}

fn unix_to_iso(unix: i64) -> String {
    use chrono::TimeZone;
    chrono::Utc.timestamp_opt(unix, 0).single().map_or_else(|| unix.to_string(), |dt| dt.to_rfc3339())
}

#[must_use]
pub fn format_bytes_human(bytes: u64) -> String {
    if bytes >= ONE_GB { format!("{:.1}GB", bytes as f64 / ONE_GB as f64) }
    else if bytes >= 1_048_576 { format!("{:.1}MB", bytes as f64 / 1_048_576.0) }
    else if bytes >= 1024 { format!("{:.1}KB", bytes as f64 / 1024.0) }
    else { format!("{bytes}B") }
}

fn string(value: &serde_json::Value, keys: &[&str]) -> String { optional_string(value, keys).unwrap_or_default() }
fn optional_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| value.get(*key)).and_then(serde_json::Value::as_str).map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned)
}

fn parse_usage_pair(value: &str) -> Result<(Option<u64>, Option<u64>), ()> {
    let mut parts = value.split('/').map(str::trim);
    let left = parts.next().ok_or(())?;
    let right = parts.next();
    Ok((parse_byte_quantity(left)?, right.map(parse_byte_quantity).transpose()?.flatten()))
}

fn parse_byte_quantity(value: &str) -> Result<Option<u64>, ()> {
    let v = value.trim();
    if v.is_empty() || v == "--" { return Ok(None); }
    let split = v.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(v.len());
    let number: f64 = v[..split].trim().parse().map_err(|_| ())?;
    let unit = v[split..].trim().to_ascii_lowercase();
    let mult = match unit.as_str() {
        "" | "b" => 1.0, "kb" => 1_000.0, "mb" => 1_000_000.0, "gb" => 1_000_000_000.0, "tb" => 1_000_000_000_000.0,
        "kib" => 1024.0, "mib" => 1024.0 * 1024.0, "gib" => 1024.0 * 1024.0 * 1024.0, "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return Err(()),
    };
    Ok(Some((number * mult).round() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> tokio::runtime::Runtime { tokio::runtime::Runtime::new().unwrap() }

    #[test]
    fn mock_client_works() {
        let client = MockDockerClient {
            containers: vec![Container { id: "abc".into(), name: "api".into(), state: "running".into(), ..Container::default() }],
            ..Default::default()
        };
        let containers = rt().block_on(client.list_containers()).unwrap();
        assert_eq!(containers[0].name, "api");
    }

    #[test]
    fn mock_inspect_not_found() {
        let err = rt().block_on(MockDockerClient::default().inspect_container("x")).unwrap_err();
        assert!(matches!(err, DockerError::NotFound(_)));
    }

    #[test]
    fn mock_ping() {
        assert!(rt().block_on(MockDockerClient::default().ping()).unwrap());
    }

    #[test]
    fn parses_usage_pair() {
        assert_eq!(parse_usage_pair("12.5MiB / 1GiB"), Ok((Some(13_107_200), Some(1_073_741_824))));
    }

    #[test]
    fn normalizes_docker_stats_json() {
        let value: serde_json::Value = serde_json::from_str(
            r#"{"Container":"abc123","Name":"api","CPUPerc":"87.50%","MemUsage":"128MiB / 1GiB","NetIO":"1.5kB / 2kB","BlockIO":"4MiB / 8MiB"}"#,
        ).unwrap();
        let sample = metric_sample_from_stats_json(&value).unwrap();
        assert_eq!(sample.container_id, "abc123");
        assert_eq!(sample.cpu_percent, Some(87.5));
    }
}
