use std::{
    collections::{HashMap, VecDeque},
    ffi::OsStr,
    process::Command,
};

use serde_json::Value;
use thiserror::Error;

use crate::docker::{DockerError, MetricSample};

pub trait MetricsCollector {
    fn collect(&self) -> Result<Vec<MetricSample>, MetricsError>;
}

#[derive(Debug, Error)]
pub enum MetricsError {
    #[error("{0}")]
    Docker(#[from] DockerError),
    #[error("invalid docker stats field `{field}`: {value}")]
    InvalidStatsField { field: &'static str, value: String },
}

#[derive(Debug, Clone)]
pub struct DockerCliMetricsCollector {
    docker_bin: String,
}

impl Default for DockerCliMetricsCollector {
    fn default() -> Self {
        Self::new("docker")
    }
}

impl DockerCliMetricsCollector {
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
        let args = args.into_iter().collect::<Vec<_>>();
        let command_display = format_command(&self.docker_bin, &args);
        let output = Command::new(&self.docker_bin)
            .args(&args)
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

impl MetricsCollector for DockerCliMetricsCollector {
    fn collect(&self) -> Result<Vec<MetricSample>, MetricsError> {
        self.run_json_lines(["stats", "--no-stream", "--format", "{{json .}}"])?
            .iter()
            .map(metric_sample_from_stats_json)
            .collect()
    }
}

#[derive(Debug, Clone, Default)]
pub struct MockMetricsCollector {
    pub samples: Vec<MetricSample>,
}

impl MetricsCollector for MockMetricsCollector {
    fn collect(&self) -> Result<Vec<MetricSample>, MetricsError> {
        Ok(self.samples.clone())
    }
}

#[derive(Debug, Clone, Default)]
pub struct NoopMetricsCollector;

impl MetricsCollector for NoopMetricsCollector {
    fn collect(&self) -> Result<Vec<MetricSample>, MetricsError> {
        Ok(Vec::new())
    }
}

#[derive(Debug, Clone, Default)]
pub struct MetricRingBuffer {
    samples: VecDeque<MetricSample>,
    capacity: usize,
}

impl MetricRingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, sample: MetricSample) {
        if self.capacity == 0 {
            return;
        }

        while self.samples.len() >= self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    pub fn latest_by_container(&self) -> HashMap<String, MetricSample> {
        let mut latest = HashMap::new();
        for sample in &self.samples {
            latest.insert(sample.container_id.clone(), sample.clone());
            latest.insert(sample.container_name.clone(), sample.clone());
        }
        latest
    }
}

pub fn metric_sample_from_stats_json(value: &Value) -> Result<MetricSample, MetricsError> {
    let mem_usage = optional_string(value, &["MemUsage"]);
    let (memory_usage_bytes, memory_limit_bytes) = mem_usage
        .as_deref()
        .map(parse_usage_pair)
        .transpose()
        .map_err(|_| MetricsError::InvalidStatsField {
            field: "MemUsage",
            value: mem_usage.unwrap_or_default(),
        })?
        .unwrap_or((None, None));

    let net_io = optional_string(value, &["NetIO"]);
    let (network_rx_bytes, network_tx_bytes) = net_io
        .as_deref()
        .map(parse_usage_pair)
        .transpose()
        .map_err(|_| MetricsError::InvalidStatsField {
            field: "NetIO",
            value: net_io.unwrap_or_default(),
        })?
        .unwrap_or((None, None));

    let block_io = optional_string(value, &["BlockIO"]);
    let (disk_read_bytes, disk_write_bytes) = block_io
        .as_deref()
        .map(parse_usage_pair)
        .transpose()
        .map_err(|_| MetricsError::InvalidStatsField {
            field: "BlockIO",
            value: block_io.unwrap_or_default(),
        })?
        .unwrap_or((None, None));

    Ok(MetricSample {
        container_id: string(value, &["ID", "Container"]),
        container_name: string(value, &["Name"]),
        timestamp: {
            let ts = string(value, &["Timestamp"]);
            if ts.is_empty() {
                chrono::Utc::now().to_rfc3339()
            } else {
                ts
            }
        },
        cpu_percent: optional_string(value, &["CPUPerc"])
            .as_deref()
            .map(parse_percent)
            .transpose()
            .map_err(|_| MetricsError::InvalidStatsField {
                field: "CPUPerc",
                value: optional_string(value, &["CPUPerc"]).unwrap_or_default(),
            })?,
        memory_usage_bytes,
        memory_limit_bytes,
        network_rx_bytes,
        network_tx_bytes,
        disk_read_bytes,
        disk_write_bytes,
    })
}

#[allow(clippy::result_unit_err)]
pub fn parse_percent(value: &str) -> Result<f64, ()> {
    value.trim().trim_end_matches('%').parse().map_err(|_| ())
}

#[allow(clippy::result_unit_err)]
pub fn parse_usage_pair(value: &str) -> Result<(Option<u64>, Option<u64>), ()> {
    let mut parts = value.split('/').map(str::trim);
    let left = parts.next().ok_or(())?;
    let right = parts.next();

    Ok((
        parse_byte_quantity(left)?,
        right.map(parse_byte_quantity).transpose()?.flatten(),
    ))
}

#[allow(clippy::result_unit_err)]
pub fn parse_byte_quantity(value: &str) -> Result<Option<u64>, ()> {
    let value = value.trim();
    if value.is_empty() || value == "--" {
        return Ok(None);
    }

    let split_at = value
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(value.len());
    let number = value[..split_at].trim().parse::<f64>().map_err(|_| ())?;
    let unit = value[split_at..].trim().to_ascii_lowercase();
    let multiplier = match unit.as_str() {
        "" | "b" => 1.0,
        "kb" => 1_000.0,
        "mb" => 1_000_000.0,
        "gb" => 1_000_000_000.0,
        "tb" => 1_000_000_000_000.0,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return Err(()),
    };

    Ok(Some((number * multiplier).round() as u64))
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

fn string(value: &Value, keys: &[&str]) -> String {
    optional_string(value, keys).unwrap_or_default()
}

fn optional_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
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

    #[test]
    fn parses_percent() {
        assert_eq!(parse_percent("85.42%"), Ok(85.42));
    }

    #[test]
    fn parses_byte_quantities() {
        assert_eq!(parse_byte_quantity("1KiB"), Ok(Some(1024)));
        assert_eq!(parse_byte_quantity("1.5MB"), Ok(Some(1_500_000)));
        assert_eq!(parse_byte_quantity("--"), Ok(None));
    }

    #[test]
    fn parses_usage_pairs() {
        assert_eq!(
            parse_usage_pair("12.5MiB / 1GiB"),
            Ok((Some(13_107_200), Some(1_073_741_824)))
        );
    }

    #[test]
    fn normalizes_docker_stats_json() {
        let value: Value = serde_json::from_str(
            r#"{"Container":"abc123","Name":"api","CPUPerc":"87.50%","MemUsage":"128MiB / 1GiB","NetIO":"1.5kB / 2kB","BlockIO":"4MiB / 8MiB"}"#,
        )
        .expect("json should parse");

        let sample = metric_sample_from_stats_json(&value).expect("sample should normalize");

        assert_eq!(sample.container_id, "abc123");
        assert_eq!(sample.container_name, "api");
        assert_eq!(sample.cpu_percent, Some(87.5));
        assert_eq!(sample.memory_usage_bytes, Some(134_217_728));
        assert_eq!(sample.memory_limit_bytes, Some(1_073_741_824));
        assert_eq!(sample.network_rx_bytes, Some(1_500));
        assert_eq!(sample.network_tx_bytes, Some(2_000));
        assert_eq!(sample.disk_read_bytes, Some(4_194_304));
        assert_eq!(sample.disk_write_bytes, Some(8_388_608));
        assert!(!sample.timestamp.is_empty(), "timestamp should have a fallback value when Docker stats omits it");
    }

    #[test]
    fn uses_provided_timestamp_when_available() {
        let value: Value = serde_json::from_str(
            r#"{"Container":"abc123","Name":"api","Timestamp":"2026-05-31T02:00:00Z","CPUPerc":"50.00%"}"#,
        )
        .expect("json should parse");

        let sample = metric_sample_from_stats_json(&value).expect("sample should normalize");

        assert_eq!(sample.timestamp, "2026-05-31T02:00:00Z");
    }

    #[test]
    fn ring_buffer_keeps_latest_samples_by_container() {
        let mut buffer = MetricRingBuffer::new(1);
        buffer.push(MetricSample {
            container_id: "abc".to_owned(),
            container_name: "api".to_owned(),
            timestamp: "t1".to_owned(),
            cpu_percent: Some(10.0),
            memory_usage_bytes: None,
            memory_limit_bytes: None,
            network_rx_bytes: None,
            network_tx_bytes: None,
            disk_read_bytes: None,
            disk_write_bytes: None,
        });
        buffer.push(MetricSample {
            container_id: "abc".to_owned(),
            container_name: "api".to_owned(),
            timestamp: "t2".to_owned(),
            cpu_percent: Some(20.0),
            memory_usage_bytes: None,
            memory_limit_bytes: None,
            network_rx_bytes: None,
            network_tx_bytes: None,
            disk_read_bytes: None,
            disk_write_bytes: None,
        });

        let latest = buffer.latest_by_container();

        assert_eq!(latest["abc"].cpu_percent, Some(20.0));
        assert_eq!(latest["api"].cpu_percent, Some(20.0));
    }
}
