use std::{ffi::OsStr, process::Command};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub trait DockerClient {
    fn list_containers(&self) -> Result<Vec<Container>, DockerError>;
    fn list_images(&self) -> Result<Vec<Image>, DockerError>;
    fn list_networks(&self) -> Result<Vec<Network>, DockerError>;
    fn list_volumes(&self) -> Result<Vec<Volume>, DockerError>;
}

pub fn list_running_containers<C>(client: &C) -> Result<Vec<Container>, DockerError>
where
    C: DockerClient + ?Sized,
{
    Ok(client
        .list_containers()?
        .into_iter()
        .filter(|container| container.state == "running")
        .collect())
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Error)]
pub enum DockerError {
    #[error("failed to run docker command `{command}`: {source}")]
    CommandIo {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("docker command `{command}` failed with exit code {code:?}: {stderr}")]
    CommandFailed {
        command: String,
        code: Option<i32>,
        stderr: String,
    },
    #[error("failed to parse docker JSON line {line_number}: {source}")]
    JsonLine {
        line_number: usize,
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Debug, Clone)]
pub struct DockerCliClient {
    docker_bin: String,
}

impl Default for DockerCliClient {
    fn default() -> Self {
        Self::new("docker")
    }
}

impl DockerCliClient {
    pub fn new(docker_bin: impl Into<String>) -> Self {
        Self {
            docker_bin: docker_bin.into(),
        }
    }

    fn run_json_lines<I, S>(&self, args: I) -> Result<Vec<Value>, DockerError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<String> = args.into_iter().map(|s| s.as_ref().to_string_lossy().into_owned()).collect();
        self.run_json_lines_str(&args)
    }

    fn run_json_lines_str(&self, args: &[String]) -> Result<Vec<Value>, DockerError> {
        let command_display = format_command_str(&self.docker_bin, args);
        let output = Command::new(&self.docker_bin)
            .args(args)
            .output()
            .map_err(|source| DockerError::CommandIo {
                command: command_display.clone(),
                source,
            })?;

        if !output.status.success() {
            return Err(DockerError::CommandFailed {
                command: command_display,
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }

        parse_json_lines(&String::from_utf8_lossy(&output.stdout))
    }
}

impl DockerClient for DockerCliClient {
    fn list_containers(&self) -> Result<Vec<Container>, DockerError> {
        let values = self.run_json_lines(["ps", "-a", "--format", "{{json .}}"])?;
        let mut containers: Vec<Container> = values
            .iter()
            .map(container_from_ps_json)
            .collect::<Result<_, _>>()?;

        if !containers.is_empty() {
            let ids: Vec<String> = containers.iter().map(|c| c.id.clone()).collect();
            let mut args: Vec<String> = vec!["inspect".to_owned(), "--format".to_owned(), "{{json .}}".to_owned()];
            args.extend(ids);
            if let Ok(inspect_values) = self.run_json_lines_str(&args) {
                enrich_containers_from_inspect(&mut containers, &inspect_values);
            }
        }

        Ok(containers)
    }

    fn list_images(&self) -> Result<Vec<Image>, DockerError> {
        self.run_json_lines(["image", "ls", "--format", "{{json .}}"])?
            .iter()
            .map(image_from_ls_json)
            .collect()
    }

    fn list_networks(&self) -> Result<Vec<Network>, DockerError> {
        self.run_json_lines(["network", "ls", "--format", "{{json .}}"])?
            .iter()
            .map(network_from_ls_json)
            .collect()
    }

    fn list_volumes(&self) -> Result<Vec<Volume>, DockerError> {
        self.run_json_lines(["volume", "ls", "--format", "{{json .}}"])?
            .iter()
            .map(volume_from_ls_json)
            .collect()
    }
}

#[derive(Debug, Clone, Default)]
pub struct MockDockerClient {
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
    pub networks: Vec<Network>,
    pub volumes: Vec<Volume>,
}

impl DockerClient for MockDockerClient {
    fn list_containers(&self) -> Result<Vec<Container>, DockerError> {
        Ok(self.containers.clone())
    }

    fn list_images(&self) -> Result<Vec<Image>, DockerError> {
        Ok(self.images.clone())
    }

    fn list_networks(&self) -> Result<Vec<Network>, DockerError> {
        Ok(self.networks.clone())
    }

    fn list_volumes(&self) -> Result<Vec<Volume>, DockerError> {
        Ok(self.volumes.clone())
    }
}

fn parse_json_lines(output: &str) -> Result<Vec<Value>, DockerError> {
    output
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|source| DockerError::JsonLine {
                line_number: index + 1,
                source,
            })
        })
        .collect()
}

fn container_from_ps_json(value: &Value) -> Result<Container, DockerError> {
    Ok(Container {
        id: get_string(value, &["ID", "Id"]),
        name: get_string(value, &["Names", "Name"]),
        image: get_string(value, &["Image"]),
        status: get_string(value, &["Status"]),
        state: get_string(value, &["State"]),
        ports: split_csv(&get_string(value, &["Ports"])),
        labels: split_csv(&get_string(value, &["Labels"])),
        created_at: get_optional_string(value, &["CreatedAt", "Created"]),
        started_at: None,
        finished_at: None,
        restart_count: None,
    })
}

fn enrich_containers_from_inspect(containers: &mut [Container], inspect_values: &[Value]) {
    let inspect_by_id: std::collections::HashMap<String, &Value> = inspect_values
        .iter()
        .filter_map(|v| {
            let id = v.get("Id").and_then(Value::as_str).unwrap_or("").to_owned();
            let short = &id[..12.min(id.len())];
            Some((short.to_owned(), v))
        })
        .collect();

    let inspect_by_name: std::collections::HashMap<String, &Value> = inspect_values
        .iter()
        .filter_map(|v| {
            let name = v.get("Name").and_then(Value::as_str).unwrap_or("").trim_start_matches('/').to_owned();
            Some((name, v))
        })
        .collect();

    for container in containers.iter_mut() {
            let inspect = inspect_by_id
                .get(&container.id[..12.min(container.id.len())])
                .or_else(|| inspect_by_name.get(&container.name))
                .map(|v| *v);

        if let Some(inspect) = inspect {
            if container.started_at.is_none() {
                container.started_at = inspect
                    .pointer("/State/StartedAt")
                    .and_then(Value::as_str)
                    .filter(|s| !s.starts_with("0001"))
                    .map(str::to_owned);
            }
            if container.finished_at.is_none() {
                container.finished_at = inspect
                    .pointer("/State/FinishedAt")
                    .and_then(Value::as_str)
                    .filter(|s| !s.starts_with("0001"))
                    .map(str::to_owned);
            }
            if container.restart_count.is_none() {
                container.restart_count = inspect
                    .pointer("/RestartCount")
                    .and_then(Value::as_u64);
            }
        }
    }
}

fn image_from_ls_json(value: &Value) -> Result<Image, DockerError> {
    Ok(Image {
        id: get_string(value, &["ID", "Id"]),
        repository: get_string(value, &["Repository"]),
        tag: get_string(value, &["Tag"]),
        digest: get_optional_string(value, &["Digest"]),
        size: get_string(value, &["Size"]),
        created_at: get_optional_string(value, &["CreatedAt", "CreatedSince"]),
        labels: split_csv(&get_string(value, &["Labels"])),
    })
}

fn network_from_ls_json(value: &Value) -> Result<Network, DockerError> {
    Ok(Network {
        id: get_string(value, &["ID", "Id"]),
        name: get_string(value, &["Name"]),
        driver: get_string(value, &["Driver"]),
        scope: get_string(value, &["Scope"]),
        containers: Vec::new(),
        labels: split_csv(&get_string(value, &["Labels"])),
    })
}

fn volume_from_ls_json(value: &Value) -> Result<Volume, DockerError> {
    Ok(Volume {
        name: get_string(value, &["Name"]),
        driver: get_string(value, &["Driver"]),
        mountpoint: get_optional_string(value, &["Mountpoint"]),
        scope: get_optional_string(value, &["Scope"]),
        labels: split_csv(&get_string(value, &["Labels"])),
    })
}

fn get_string(value: &Value, keys: &[&str]) -> String {
    get_optional_string(value, keys).unwrap_or_default()
}

fn get_optional_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "<none>")
        .map(str::to_owned)
}

fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

fn format_command_str(bin: &str, args: &[String]) -> String {
    let mut parts = vec![bin.to_owned()];
    parts.extend(args.iter().cloned());
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_client_returns_normalized_entities() {
        let client = MockDockerClient {
            containers: vec![Container {
                id: "abc123".to_owned(),
                name: "api-service".to_owned(),
                image: "api:latest".to_owned(),
                status: "Up 2 minutes".to_owned(),
                state: "running".to_owned(),
                ports: vec!["8080/tcp".to_owned()],
                labels: vec!["com.example.role=api".to_owned()],
                created_at: Some("2026-05-31 02:00:00 +0300 +03".to_owned()),
                started_at: None,
                finished_at: None,
                restart_count: Some(0),
            }],
            ..Default::default()
        };

        let containers = client.list_containers().expect("mock should not fail");

        assert_eq!(containers[0].name, "api-service");
        assert_eq!(containers[0].state, "running");
    }

    #[test]
    fn lists_running_containers_from_any_client() {
        let client = MockDockerClient {
            containers: vec![
                Container {
                    id: "abc123".to_owned(),
                    name: "api-service".to_owned(),
                    image: "api:latest".to_owned(),
                    status: "Up 2 minutes".to_owned(),
                    state: "running".to_owned(),
                    ports: Vec::new(),
                    labels: Vec::new(),
                    created_at: None,
                    started_at: None,
                    finished_at: None,
                    restart_count: Some(0),
                },
                Container {
                    id: "def456".to_owned(),
                    name: "worker".to_owned(),
                    image: "worker:latest".to_owned(),
                    status: "Exited (0) 1 hour ago".to_owned(),
                    state: "exited".to_owned(),
                    ports: Vec::new(),
                    labels: Vec::new(),
                    created_at: None,
                    started_at: None,
                    finished_at: None,
                    restart_count: Some(0),
                },
            ],
            ..Default::default()
        };

        let running = list_running_containers(&client).expect("mock should not fail");

        assert_eq!(running.len(), 1);
        assert_eq!(running[0].name, "api-service");
    }

    #[test]
    fn parses_docker_ps_json_lines() {
        let values = parse_json_lines(
            r#"{"ID":"abc123","Image":"postgres:16","Names":"db","State":"running","Status":"Up 1 minute","Ports":"5432/tcp","Labels":"env=dev,tier=data","CreatedAt":"2026-05-31 02:00:00 +0300 +03"}"#,
        )
        .expect("json should parse");

        let container = container_from_ps_json(&values[0]).expect("container should normalize");

        assert_eq!(container.id, "abc123");
        assert_eq!(container.name, "db");
        assert_eq!(container.image, "postgres:16");
        assert_eq!(container.ports, vec!["5432/tcp"]);
        assert_eq!(container.labels, vec!["env=dev", "tier=data"]);
    }

    #[test]
    fn parses_image_json_line() {
        let value: Value = serde_json::from_str(
            r#"{"ID":"sha256:abc","Repository":"postgres","Tag":"16","Size":"432MB","CreatedSince":"2 weeks ago"}"#,
        )
        .expect("json should parse");

        let image = image_from_ls_json(&value).expect("image should normalize");

        assert_eq!(image.repository, "postgres");
        assert_eq!(image.tag, "16");
        assert_eq!(image.created_at.as_deref(), Some("2 weeks ago"));
    }

    #[test]
    fn parses_network_json_line() {
        let value: Value = serde_json::from_str(
            r#"{"ID":"net123","Name":"bridge","Driver":"bridge","Scope":"local"}"#,
        )
        .expect("json should parse");

        let network = network_from_ls_json(&value).expect("network should normalize");

        assert_eq!(network.name, "bridge");
        assert_eq!(network.driver, "bridge");
        assert_eq!(network.scope, "local");
    }

    #[test]
    fn parses_volume_json_line() {
        let value: Value = serde_json::from_str(
            r#"{"Name":"pgdata","Driver":"local","Mountpoint":"/var/lib/docker/volumes/pgdata/_data","Scope":"local"}"#,
        )
        .expect("json should parse");

        let volume = volume_from_ls_json(&value).expect("volume should normalize");

        assert_eq!(volume.name, "pgdata");
        assert_eq!(volume.driver, "local");
        assert_eq!(volume.scope.as_deref(), Some("local"));
    }
}
