use serde_json::Value as JsonValue;

use crate::executor::ExecutionResult;

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
pub enum ExportFormat {
    Influx,
    Loki,
    Prometheus,
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("serialization error: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("export target returned {status}: {body}")]
    BadStatus { status: u16, body: String },
}

/// Push a query result to InfluxDB v1/v2 HTTP write API.
///
/// `url` should be the full write endpoint, e.g.:
///   `http://localhost:8086/write?db=mydb` (InfluxDB v1)
///   `http://localhost:8086/api/v2/write?org=myorg&bucket=mybucket` (v2)
pub async fn push_to_influxdb(url: &str, result: &ExecutionResult) -> Result<(), ExportError> {
    let body = format_as_influx(result, "containers");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let resp = client
        .post(url)
        .header("Content-Type", "application/octet-stream")
        .body(body)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(ExportError::BadStatus {
            status: status.as_u16(),
            body,
        });
    }
    Ok(())
}

/// Push a query result to Grafana Loki HTTP push API.
///
/// `url` should be the base Loki URL, e.g. `http://localhost:3100`.
/// The push endpoint `/loki/api/v1/push` is appended automatically.
pub async fn push_to_loki(url: &str, result: &ExecutionResult) -> Result<(), ExportError> {
    let body = format_as_loki(result)?;
    let push_url = format!("{}/loki/api/v1/push", url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let resp = client
        .post(&push_url)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(ExportError::BadStatus {
            status: status.as_u16(),
            body,
        });
    }
    Ok(())
}

/// Push a query result to Prometheus Pushgateway.
///
/// `url` should be the Pushgateway URL, e.g. `http://localhost:9091`.
/// The job name is fixed as `dol`.
pub async fn push_to_prometheus(url: &str, result: &ExecutionResult) -> Result<(), ExportError> {
    let body = format_as_prometheus(result);
    let push_url = format!("{}/metrics/job/dol", url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let resp = client
        .put(&push_url)
        .header("Content-Type", "text/plain; version=0.0.4")
        .body(body)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(ExportError::BadStatus {
            status: status.as_u16(),
            body,
        });
    }
    Ok(())
}

/// Format execution result as InfluxDB line protocol.
///
/// Each row becomes a line in the format:
///   `<measurement>,name=<name>,image=<image>,state=<state> <field_kv>`
pub fn format_as_influx(result: &ExecutionResult, measurement: &str) -> String {
    let mut lines = Vec::new();
    for row in &result.rows {
        let name = row
            .fields
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let image = row
            .fields
            .get("image")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let state = row
            .fields
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let mut fields = Vec::new();
        let mut tags = vec![
            format!("name={}", escape_influx_tag(name)),
            format!("image={}", escape_influx_tag(image)),
            format!("state={}", escape_influx_tag(state)),
        ];

        // Also add other string fields as tags
        for (key, val) in &row.fields {
            match key.as_str() {
                "name" | "image" | "state" | "id" | "status" | "ports" => {
                    if let Some(s) = val.as_str() {
                        tags.push(format!("{key}={}", escape_influx_tag(s)));
                    }
                }
                "cpu" | "memory" | "memory_limit" | "restart_count" | "network_rx"
                | "network_tx" | "disk_read" | "disk_write" | "size" | "count" => {
                    if let Some(n) = val.as_f64() {
                        fields.push(format!("{key}={n}"));
                    }
                }
                _ => {}
            }
        }

        if fields.is_empty() {
            fields.push("_value=1".to_owned());
        }

        let line = format!(
            "{measurement},{tags} {fields}",
            tags = tags.join(","),
            fields = fields.join(",")
        );
        lines.push(line);
    }
    lines.join("\n") + "\n"
}

/// Format execution result as Loki JSON push payload.
pub fn format_as_loki(result: &ExecutionResult) -> Result<String, serde_json::Error> {
    let mut entries: Vec<Vec<JsonValue>> = Vec::new();
    for row in &result.rows {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .to_string();
        let log_line = serde_json::to_string(&row.fields)?;
        entries.push(vec![JsonValue::String(ts), JsonValue::String(log_line)]);
    }

    let payload = serde_json::json!({
        "streams": [{
            "stream": {
                "app": "dol",
                "source": "docker"
            },
            "values": entries
        }]
    });
    serde_json::to_string(&payload)
}

/// Format execution result as Prometheus exposition format.
///
/// Numeric fields become gauge metrics:
///   `dol_<field>{container="<name>",image="<image>",state="<state>"} <value>`
pub fn format_as_prometheus(result: &ExecutionResult) -> String {
    let mut lines = Vec::new();
    lines.push("# HELP dol_query Docker query results from DOL".to_owned());
    lines.push("# TYPE dol_query gauge".to_owned());

    for row in &result.rows {
        let name = row
            .fields
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let image = row
            .fields
            .get("image")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let state = row
            .fields
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let labels = format!(
            "container=\"{}\",image=\"{}\",state=\"{}\"",
            escape_prom_label(name),
            escape_prom_label(image),
            escape_prom_label(state),
        );

        for (key, val) in &row.fields {
            if let Some(n) = val.as_f64() {
                lines.push(format!("dol_{key}{{{labels}}} {n}"));
            } else if let Some(i) = val.as_i64() {
                lines.push(format!("dol_{key}{{{labels}}} {i}"));
            }
        }
    }

    lines.join("\n") + "\n"
}

fn escape_influx_tag(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(',', "\\,")
        .replace('=', "\\=")
        .replace(' ', "\\ ")
}

fn escape_prom_label(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::Row;

    fn sample_result() -> ExecutionResult {
        ExecutionResult {
            rows: vec![
                Row {
                    fields: std::collections::BTreeMap::from([
                        ("name".to_owned(), JsonValue::String("web".to_owned())),
                        (
                            "image".to_owned(),
                            JsonValue::String("nginx:latest".to_owned()),
                        ),
                        ("state".to_owned(), JsonValue::String("running".to_owned())),
                        (
                            "cpu".to_owned(),
                            JsonValue::Number(serde_json::Number::from_f64(12.5).unwrap()),
                        ),
                        (
                            "memory".to_owned(),
                            JsonValue::Number(serde_json::Number::from_f64(64_000_000.0).unwrap()),
                        ),
                        (
                            "restart_count".to_owned(),
                            JsonValue::Number(serde_json::Number::from(0)),
                        ),
                    ]),
                },
                Row {
                    fields: std::collections::BTreeMap::from([
                        ("name".to_owned(), JsonValue::String("db".to_owned())),
                        (
                            "image".to_owned(),
                            JsonValue::String("postgres:16".to_owned()),
                        ),
                        ("state".to_owned(), JsonValue::String("exited".to_owned())),
                        (
                            "cpu".to_owned(),
                            JsonValue::Number(serde_json::Number::from_f64(0.0).unwrap()),
                        ),
                        (
                            "memory".to_owned(),
                            JsonValue::Number(serde_json::Number::from_f64(256_000_000.0).unwrap()),
                        ),
                        (
                            "restart_count".to_owned(),
                            JsonValue::Number(serde_json::Number::from(3)),
                        ),
                    ]),
                },
            ],
        }
    }

    #[test]
    fn formats_influx_line_protocol() {
        let result = sample_result();
        let output = format_as_influx(&result, "containers");
        assert!(output.contains("containers,"));
        assert!(output.contains("name=web"));
        assert!(output.contains("name=db"));
        assert!(output.contains("cpu=12.5"));
        assert!(output.contains("restart_count=3"));
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn formats_loki_json() {
        let result = sample_result();
        let output = format_as_loki(&result).unwrap();
        assert!(output.contains(r#""app":"dol""#));
        assert!(output.contains(r#""streams""#));
        assert!(output.contains(r#""values""#));
    }

    #[test]
    fn formats_prometheus_exposition() {
        let result = sample_result();
        let output = format_as_prometheus(&result);
        assert!(output.contains("dol_cpu{"));
        assert!(output.contains("dol_memory{"));
        assert!(output.contains("container=\"web\""));
        assert!(output.contains("container=\"db\""));
        assert!(output.contains("# HELP dol_query"));
    }
}
