use std::{
    collections::{BTreeMap, HashMap},
    time::{Duration as StdDuration, Instant},
};

use serde::Serialize;
use serde_json::{Number, Value as JsonValue};
use thiserror::Error;

use crate::{
    ast::{AlertAction, AlertRule, Duration, DurationUnit},
    docker::MetricSample,
    eval::{self, EvalError},
    metrics::{MetricsCollector, MetricsError},
};

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct AlertEvent {
    pub container_id: String,
    pub container_name: String,
    pub message: String,
    pub action: AlertActionPlan,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub enum AlertActionPlan {
    Print { message: String },
    Webhook { url: String, executed: bool },
    Restart { target: String, executed: bool },
}

#[derive(Debug, Error)]
pub enum AlertError {
    #[error("{0}")]
    Metrics(#[from] MetricsError),
    #[error("{0}")]
    Eval(#[from] EvalError),
    #[error("HTTP request failed: {0}")]
    Http(String),
    #[error("Docker restart failed: {0}")]
    Restart(String),
}

#[derive(Debug, Default)]
pub struct AlertEvaluator {
    active_since: HashMap<String, Instant>,
}

impl AlertEvaluator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn evaluate_samples(
        &mut self,
        rule: &AlertRule,
        samples: &[MetricSample],
        now: Instant,
    ) -> Result<Vec<AlertEvent>, AlertError> {
        let mut events = Vec::new();

        for sample in samples {
            let key = sample_key(sample);
            let row = sample_fields(sample);
            let matches = eval::evaluate_expression(&row, &rule.condition)?;

            if !matches {
                self.active_since.remove(&key);
                continue;
            }

            let since = *self.active_since.entry(key).or_insert(now);
            let elapsed = now.saturating_duration_since(since);
            let required = rule.duration.map(duration_to_std).unwrap_or_default();

            if elapsed >= required {
                events.push(alert_event(rule, sample)?);
            }
        }

        Ok(events)
    }
}

pub fn evaluate_alert_once<C>(
    rule: &AlertRule,
    collector: &C,
) -> Result<Vec<AlertEvent>, AlertError>
where
    C: MetricsCollector + ?Sized,
{
    let mut evaluator = AlertEvaluator::new();
    let samples = collector.collect()?;
    evaluator.evaluate_samples(rule, &samples, Instant::now())
}

pub fn render_alert_event(event: &AlertEvent) -> String {
    match &event.action {
        AlertActionPlan::Print { message } => {
            format!(
                "{} [{}]: {}",
                event.container_name, event.container_id, message
            )
        }
        AlertActionPlan::Webhook { url, executed } => {
            if *executed {
                format!(
                    "{} [{}]: POSTED alert to {}",
                    event.container_name, event.container_id, url
                )
            } else {
                format!(
                    "{} [{}]: FAILED to POST alert to {}",
                    event.container_name, event.container_id, url
                )
            }
        }
        AlertActionPlan::Restart { target, executed } => {
            if *executed {
                format!(
                    "{} [{}]: RESTARTED {}",
                    event.container_name, event.container_id, target
                )
            } else {
                format!(
                    "{} [{}]: FAILED to restart {}",
                    event.container_name, event.container_id, target
                )
            }
        }
    }
}

pub fn duration_to_std(duration: Duration) -> StdDuration {
    let seconds = match duration.unit {
        DurationUnit::Seconds => duration.value,
        DurationUnit::Minutes => duration.value * 60,
        DurationUnit::Hours => duration.value * 60 * 60,
        DurationUnit::Days => duration.value * 60 * 60 * 24,
    };
    StdDuration::from_secs(seconds)
}

fn alert_event(rule: &AlertRule, sample: &MetricSample) -> Result<AlertEvent, AlertError> {
    let action = match &rule.action {
        AlertAction::Print(message) => AlertActionPlan::Print {
            message: message.clone(),
        },
        AlertAction::Webhook(url) => {
            let executed = execute_webhook(url);
            AlertActionPlan::Webhook {
                url: url.clone(),
                executed,
            }
        }
        AlertAction::Restart(target) => {
            let target_str = format!("{:?} {}", target.kind, target.value);
            let executed = execute_restart(&target_str);
            AlertActionPlan::Restart {
                target: target_str,
                executed,
            }
        }
    };
    let message = render_action_message(&action);

    Ok(AlertEvent {
        container_id: sample.container_id.clone(),
        container_name: sample.container_name.clone(),
        message,
        action,
    })
}

fn execute_webhook(url: &str) -> bool {
    let rt = tokio::runtime::Runtime::new();
    match rt {
        Ok(runtime) => {
            match runtime.block_on(async {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(10))
                    .build()?;
                let resp = client.post(url).send().await?;
                Ok::<_, reqwest::Error>(resp.status().is_success())
            }) {
                Ok(success) => success,
                Err(e) => {
                    eprintln!("Webhook POST failed: {e}");
                    false
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to create runtime for webhook: {e}");
            false
        }
    }
}

fn execute_restart(target: &str) -> bool {
    let target = target.trim();
    let output = std::process::Command::new("docker")
        .args(["restart", target])
        .output();
    match output {
        Ok(out) => {
            if out.status.success() {
                true
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr);
                eprintln!("docker restart failed: {stderr}");
                false
            }
        }
        Err(e) => {
            eprintln!("Failed to execute docker restart: {e}");
            false
        }
    }
}

fn render_action_message(action: &AlertActionPlan) -> String {
    match action {
        AlertActionPlan::Print { message } => message.clone(),
        AlertActionPlan::Webhook { url, executed } => {
            if *executed {
                format!("webhook alert sent to {url}")
            } else {
                format!("webhook alert FAILED for {url}")
            }
        }
        AlertActionPlan::Restart { target, executed } => {
            if *executed {
                format!("restarted {target}")
            } else {
                format!("restart FAILED for {target}")
            }
        }
    }
}

fn sample_key(sample: &MetricSample) -> String {
    if sample.container_id.is_empty() {
        sample.container_name.clone()
    } else {
        sample.container_id.clone()
    }
}

fn sample_fields(sample: &MetricSample) -> BTreeMap<String, JsonValue> {
    BTreeMap::from([
        (
            "container_id".to_owned(),
            JsonValue::String(sample.container_id.clone()),
        ),
        (
            "container_name".to_owned(),
            JsonValue::String(sample.container_name.clone()),
        ),
        (
            "name".to_owned(),
            JsonValue::String(sample.container_name.clone()),
        ),
        (
            "timestamp".to_owned(),
            JsonValue::String(sample.timestamp.clone()),
        ),
        ("cpu".to_owned(), json_option_f64(sample.cpu_percent)),
        (
            "memory".to_owned(),
            json_option_u64(sample.memory_usage_bytes),
        ),
        (
            "memory_limit".to_owned(),
            json_option_u64(sample.memory_limit_bytes),
        ),
        (
            "network_rx".to_owned(),
            json_option_u64(sample.network_rx_bytes),
        ),
        (
            "network_tx".to_owned(),
            json_option_u64(sample.network_tx_bytes),
        ),
        (
            "disk_read".to_owned(),
            json_option_u64(sample.disk_read_bytes),
        ),
        (
            "disk_write".to_owned(),
            json_option_u64(sample.disk_write_bytes),
        ),
    ])
}

fn json_option_f64(value: Option<f64>) -> JsonValue {
    value
        .and_then(Number::from_f64)
        .map(JsonValue::Number)
        .unwrap_or(JsonValue::Null)
}

fn json_option_u64(value: Option<u64>) -> JsonValue {
    value
        .map(Number::from)
        .map(JsonValue::Number)
        .unwrap_or(JsonValue::Null)
}

#[cfg(test)]
mod tests {
    use std::time::Duration as StdDuration;

    use crate::{ast::DurationUnit, metrics::MockMetricsCollector, parser};

    use super::*;

    #[test]
    fn fires_print_alert_without_duration() {
        let rule = alert_rule("alert when cpu > 80% then print \"High CPU\"");
        let collector = MockMetricsCollector {
            samples: vec![sample("api", 87.5)],
        };

        let events = evaluate_alert_once(&rule, &collector).expect("alert should evaluate");

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].action,
            AlertActionPlan::Print {
                message: "High CPU".to_owned()
            }
        );
    }

    #[test]
    fn honors_duration_guard() {
        let mut rule = alert_rule("alert when cpu > 80% for 2m then print \"High CPU\"");
        rule.duration = Some(Duration {
            value: 2,
            unit: DurationUnit::Minutes,
        });
        let mut evaluator = AlertEvaluator::new();
        let samples = vec![sample("api", 90.0)];
        let start = Instant::now();

        let first = evaluator
            .evaluate_samples(&rule, &samples, start)
            .expect("first evaluation should pass");
        let second = evaluator
            .evaluate_samples(&rule, &samples, start + StdDuration::from_secs(121))
            .expect("second evaluation should pass");

        assert!(first.is_empty());
        assert_eq!(second.len(), 1);
    }

    #[test]
    fn clears_duration_guard_when_condition_recovers() {
        let rule = alert_rule("alert when cpu > 80% for 2m then print \"High CPU\"");
        let mut evaluator = AlertEvaluator::new();
        let start = Instant::now();

        evaluator
            .evaluate_samples(&rule, &[sample("api", 90.0)], start)
            .expect("evaluation should pass");
        evaluator
            .evaluate_samples(
                &rule,
                &[sample("api", 20.0)],
                start + StdDuration::from_secs(60),
            )
            .expect("evaluation should pass");
        let events = evaluator
            .evaluate_samples(
                &rule,
                &[sample("api", 90.0)],
                start + StdDuration::from_secs(121),
            )
            .expect("evaluation should pass");

        assert!(events.is_empty());
    }

    #[test]
    fn plans_webhook_and_restart_as_actions() {
        let webhook = alert_rule("alert when cpu > 80% then webhook \"http://localhost/hook\"");
        let restart = alert_rule("alert when cpu > 80% then restart container api");
        let collector = MockMetricsCollector {
            samples: vec![sample("api", 90.0)],
        };

        let webhook_events = evaluate_alert_once(&webhook, &collector).expect("webhook");
        let restart_events = evaluate_alert_once(&restart, &collector).expect("restart");

        assert!(matches!(
            webhook_events[0].action,
            AlertActionPlan::Webhook { .. }
        ));
        assert!(matches!(
            restart_events[0].action,
            AlertActionPlan::Restart { .. }
        ));
    }

    fn alert_rule(source: &str) -> AlertRule {
        let parsed = parser::parse(source).expect("alert should parse");
        let crate::ast::Query::Alert(rule) = parsed.query else {
            panic!("expected alert rule");
        };
        rule
    }

    fn sample(name: &str, cpu: f64) -> MetricSample {
        MetricSample {
            container_id: format!("{name}-id"),
            container_name: name.to_owned(),
            timestamp: "2026-05-31T02:00:00Z".to_owned(),
            cpu_percent: Some(cpu),
            memory_usage_bytes: Some(128),
            memory_limit_bytes: Some(1024),
            network_rx_bytes: Some(1),
            network_tx_bytes: Some(2),
            disk_read_bytes: Some(3),
            disk_write_bytes: Some(4),
        }
    }
}
