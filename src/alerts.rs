//! Alert evaluation engine.
//!
//! Evaluates DOL alert rules against live metric samples. Supports duration
//! guards (e.g. "for 2m") and actions: `print`, `webhook` (HTTP POST), and
//! `restart` (container restart via bollard).
//!
//! # Example
//!
//! ```ignore
//! let rule = parser::parse(r#"alert when cpu > 80% for 2m then print "High""#)?;
//! let mut evaluator = AlertEvaluator::new();
//! let events = evaluator.evaluate_samples(&rule, &samples, Instant::now())?;
//! ```

use std::{
    collections::{BTreeMap, HashMap},
    sync::OnceLock,
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

// ── Global alert timeout configuration ─────────────────────────────────────

/// Global cache for alert timeout values (set once at startup from config).
static ALERT_TIMEOUTS: OnceLock<AlertTimeouts> = OnceLock::new();

struct AlertTimeouts {
    webhook: StdDuration,
    restart: StdDuration,
}

/// Initialise the global alert timeout cache. Should be called once at startup.
pub fn init_alert_timeouts(webhook_secs: u64, restart_secs: u64) {
    let _ = ALERT_TIMEOUTS.set(AlertTimeouts {
        webhook: StdDuration::from_secs(webhook_secs.max(1)),
        restart: StdDuration::from_secs(restart_secs.max(1)),
    });
}

fn webhook_timeout() -> StdDuration {
    ALERT_TIMEOUTS
        .get()
        .map_or_else(|| StdDuration::from_secs(10), |t| t.webhook)
}

fn restart_timeout() -> StdDuration {
    ALERT_TIMEOUTS
        .get()
        .map_or_else(|| StdDuration::from_secs(30), |t| t.restart)
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct AlertEvent {
    pub container_id: String,
    pub container_name: String,
    pub message: String,
    pub action: AlertActionPlan,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub enum AlertActionPlan {
    Print {
        message: String,
    },
    Webhook {
        url: String,
        executed: bool,
    },
    Restart {
        target: String,
        executed: bool,
    },
    Slack {
        url: String,
        executed: bool,
    },
    Discord {
        url: String,
        executed: bool,
    },
    Email {
        to: String,
        subject: String,
        executed: bool,
    },
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
    #[must_use]
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
                events.push(alert_event(rule, sample));
            }
        }

        Ok(events)
    }
}

pub async fn evaluate_alert_once<C>(
    rule: &AlertRule,
    collector: &C,
) -> Result<Vec<AlertEvent>, AlertError>
where
    C: MetricsCollector + ?Sized,
{
    let mut evaluator = AlertEvaluator::new();
    let samples = collector.collect().await?;
    evaluator.evaluate_samples(rule, &samples, Instant::now())
}

#[must_use]
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
        AlertActionPlan::Slack { url, executed } => {
            if *executed {
                format!(
                    "{} [{}]: POSTED Slack alert to {}",
                    event.container_name, event.container_id, url
                )
            } else {
                format!(
                    "{} [{}]: FAILED to POST Slack alert to {}",
                    event.container_name, event.container_id, url
                )
            }
        }
        AlertActionPlan::Discord { url, executed } => {
            if *executed {
                format!(
                    "{} [{}]: POSTED Discord alert to {}",
                    event.container_name, event.container_id, url
                )
            } else {
                format!(
                    "{} [{}]: FAILED to POST Discord alert to {}",
                    event.container_name, event.container_id, url
                )
            }
        }
        AlertActionPlan::Email {
            to,
            subject,
            executed,
        } => {
            if *executed {
                format!(
                    "{} [{}]: SENT email \"{}\" to {}",
                    event.container_name, event.container_id, subject, to
                )
            } else {
                format!(
                    "{} [{}]: FAILED to send email \"{}\" to {}",
                    event.container_name, event.container_id, subject, to
                )
            }
        }
    }
}

#[must_use]
pub const fn duration_to_std(duration: Duration) -> StdDuration {
    let seconds = match duration.unit {
        DurationUnit::Seconds => duration.value,
        DurationUnit::Minutes => duration.value * 60,
        DurationUnit::Hours => duration.value * 60 * 60,
        DurationUnit::Days => duration.value * 60 * 60 * 24,
    };
    StdDuration::from_secs(seconds)
}

fn alert_event(rule: &AlertRule, sample: &MetricSample) -> AlertEvent {
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
        AlertAction::Slack(url) => {
            let executed = execute_slack(url, &sample_key(sample));
            AlertActionPlan::Slack {
                url: url.clone(),
                executed,
            }
        }
        AlertAction::Discord(url) => {
            let executed = execute_discord(url, &sample_key(sample));
            AlertActionPlan::Discord {
                url: url.clone(),
                executed,
            }
        }
        AlertAction::Email { to, subject } => {
            let executed = execute_email(to, subject, &sample_key(sample));
            AlertActionPlan::Email {
                to: to.clone(),
                subject: subject.clone(),
                executed,
            }
        }
    };
    let message = render_action_message(&action);

    AlertEvent {
        container_id: sample.container_id.clone(),
        container_name: sample.container_name.clone(),
        message,
        action,
    }
}

fn execute_webhook(url: &str) -> bool {
    execute_webhook_payload(url, &serde_json::json!({}))
}

fn execute_slack(url: &str, container: &str) -> bool {
    let message = format!(
        "[DockQL Alert] Container `{}` triggered an alert",
        container
    );
    let payload = serde_json::json!({
        "text": message,
        "blocks": [
            {
                "type": "header",
                "text": {
                    "type": "plain_text",
                    "text": "DockQL Alert"
                }
            },
            {
                "type": "section",
                "fields": [
                    {
                        "type": "mrkdwn",
                        "text": format!("*Container:* {}", container)
                    },
                    {
                        "type": "mrkdwn",
                        "text": format!("*Time:* {}", chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"))
                    }
                ]
            }
        ]
    });
    execute_webhook_payload(url, &payload)
}

fn execute_discord(url: &str, container: &str) -> bool {
    let payload = serde_json::json!({
        "content": format!("DockQL Alert for `{}`", container),
        "embeds": [
            {
                "title": "DockQL Alert",
                "description": format!("Container **{}** triggered an alert", container),
                "color": 0xFF0000,
                "fields": [
                    {
                        "name": "Container",
                        "value": container,
                        "inline": true
                    },
                    {
                        "name": "Time",
                        "value": chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                        "inline": true
                    }
                ],
                "footer": {
                    "text": "DockQL"
                },
                "timestamp": chrono::Utc::now().to_rfc3339()
            }
        ]
    });
    execute_webhook_payload(url, &payload)
}

fn execute_email(to: &str, subject: &str, container: &str) -> bool {
    use lettre::{
        AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
        transport::smtp::authentication::Credentials,
    };

    let body = format!(
        "DockQL Alert\n\nContainer: {}\nSubject: {}\nTime: {}\n\nThis is an automated alert from DockQL.\n",
        container,
        subject,
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
    );

    let email = match Message::builder()
        .from(
            "DockQL <noreply@dockql>"
                .parse()
                .expect("hardcoded DockQL noreply email address"),
        )
        .to(to.parse().expect("invalid recipient email address"))
        .subject(subject)
        .body(body)
    {
        Ok(msg) => msg,
        Err(e) => {
            eprintln!("Failed to build email: {e}");
            return false;
        }
    };

    // Try config file first, then environment variables, then defaults
    let cfg = crate::config::DolConfig::load();
    let smtp_host = cfg
        .smtp_host
        .clone()
        .or_else(|| std::env::var("DOCKQL_SMTP_HOST").ok())
        .unwrap_or_else(|| "localhost".to_owned());
    let smtp_port: u16 = cfg
        .smtp_port
        .or_else(|| {
            std::env::var("DOCKQL_SMTP_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
        })
        .unwrap_or(25);
    let smtp_user = cfg
        .smtp_user
        .clone()
        .or_else(|| std::env::var("DOCKQL_SMTP_USER").ok());
    let smtp_pass = cfg
        .smtp_pass
        .clone()
        .or_else(|| std::env::var("DOCKQL_SMTP_PASS").ok());

    async fn do_email(
        email: Message,
        host: String,
        port: u16,
        user: Option<String>,
        pass: Option<String>,
    ) -> Result<bool, String> {
        let creds = match (user, pass) {
            (Some(u), Some(p)) => Some(Credentials::new(u, p)),
            _ => None,
        };

        let mailer = match creds {
            Some(c) => AsyncSmtpTransport::<Tokio1Executor>::relay(&host)
                .map_err(|e| format!("SMTP relay setup: {e}"))?
                .port(port)
                .credentials(c)
                .build(),
            None => AsyncSmtpTransport::<Tokio1Executor>::relay(&host)
                .map_err(|e| format!("SMTP relay setup: {e}"))?
                .port(port)
                .build(),
        };

        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            mailer
                .send(email)
                .await
                .map_err(|e| format!("SMTP send: {e}"))?;
            Ok(true)
        })
        .await
        .map_err(|_| "SMTP send timed out".to_owned())?
    }

    match std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("Failed to create runtime for email: {e}");
                return Ok(false);
            }
        };
        rt.block_on(do_email(email, smtp_host, smtp_port, smtp_user, smtp_pass))
    })
    .join()
    {
        Ok(Ok(success)) => success,
        Ok(Err(e)) => {
            eprintln!("Email send failed: {e}");
            false
        }
        Err(_) => {
            eprintln!("Email thread panicked");
            false
        }
    }
}

/// POST a JSON payload to a webhook URL.
fn execute_webhook_payload(url: &str, payload: &serde_json::Value) -> bool {
    async fn do_post(
        url: &str,
        payload: &serde_json::Value,
        timeout: StdDuration,
    ) -> Result<bool, String> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| format!("reqwest client: {e}"))?;
        let resp = client
            .post(url)
            .json(payload)
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        Ok(resp.status().is_success())
    }

    let timeout = webhook_timeout();
    let url = url.to_owned();
    let payload = payload.clone();
    match std::thread::spawn(move || match tokio::runtime::Runtime::new() {
        Ok(rt) => rt.block_on(do_post(&url, &payload, timeout)),
        Err(e) => {
            eprintln!("Failed to create runtime: {e}");
            Ok(false)
        }
    })
    .join()
    {
        Ok(Ok(success)) => success,
        Ok(Err(e)) => {
            eprintln!("Webhook POST failed: {e}");
            false
        }
        Err(_) => {
            eprintln!("Webhook thread panicked");
            false
        }
    }
}

fn execute_restart(target: &str) -> bool {
    async fn do_restart(target: &str, timeout: StdDuration) -> Result<bool, String> {
        let docker = bollard::Docker::connect_with_local_defaults()
            .map_err(|e| format!("bollard connect: {e}"))?;
        tokio::time::timeout(
            timeout,
            docker.restart_container(
                target,
                None::<bollard::query_parameters::RestartContainerOptions>,
            ),
        )
        .await
        .map_err(|_| format!("restart timed out after {}s", timeout.as_secs()))?
        .map_err(|e| format!("restart failed: {e}"))?;
        Ok(true)
    }

    let timeout = restart_timeout();
    let target = target.to_owned();
    match std::thread::spawn(move || match tokio::runtime::Runtime::new() {
        Ok(rt) => rt.block_on(do_restart(&target, timeout)),
        Err(e) => {
            eprintln!("Failed to create runtime for restart: {e}");
            Ok(false)
        }
    })
    .join()
    {
        Ok(Ok(success)) => success,
        Ok(Err(e)) => {
            eprintln!("Container restart failed: {e}");
            false
        }
        Err(_) => {
            eprintln!("Restart thread panicked");
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
        AlertActionPlan::Slack { url, executed } => {
            if *executed {
                format!("Slack alert sent to {url}")
            } else {
                format!("Slack alert FAILED for {url}")
            }
        }
        AlertActionPlan::Discord { url, executed } => {
            if *executed {
                format!("Discord alert sent to {url}")
            } else {
                format!("Discord alert FAILED for {url}")
            }
        }
        AlertActionPlan::Email {
            to,
            subject,
            executed,
        } => {
            if *executed {
                format!("email \"{subject}\" sent to {to}")
            } else {
                format!("email \"{subject}\" FAILED for {to}")
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
        .map_or(JsonValue::Null, JsonValue::Number)
}

fn json_option_u64(value: Option<u64>) -> JsonValue {
    value
        .map(Number::from)
        .map_or(JsonValue::Null, JsonValue::Number)
}

#[cfg(test)]
mod tests {
    use std::time::Duration as StdDuration;

    use crate::{ast::DurationUnit, metrics::MockMetricsCollector, parser};

    /// 121 seconds — just past the 2-minute duration guard used in tests.
    const JUST_PAST_TWO_MINUTES: StdDuration = StdDuration::from_secs(121);
    /// 60 seconds — used when checking that a recovered condition resets the guard.
    const ONE_MINUTE: StdDuration = StdDuration::from_secs(60);

    use super::*;

    #[test]
    fn fires_print_alert_without_duration() {
        let rule = alert_rule("alert when cpu > 80% then print \"High CPU\"");
        let collector = MockMetricsCollector {
            samples: vec![sample("api", 87.5)],
        };

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let events = rt
            .block_on(evaluate_alert_once(&rule, &collector))
            .expect("alert should evaluate");

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
            .evaluate_samples(&rule, &samples, start + JUST_PAST_TWO_MINUTES)
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
            .evaluate_samples(&rule, &[sample("api", 20.0)], start + ONE_MINUTE)
            .expect("evaluation should pass");
        let events = evaluator
            .evaluate_samples(&rule, &[sample("api", 90.0)], start + JUST_PAST_TWO_MINUTES)
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

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let webhook_events = rt
            .block_on(evaluate_alert_once(&webhook, &collector))
            .expect("webhook");
        let restart_events = rt
            .block_on(evaluate_alert_once(&restart, &collector))
            .expect("restart");

        assert!(matches!(
            webhook_events[0].action,
            AlertActionPlan::Webhook { .. }
        ));
        assert!(matches!(
            restart_events[0].action,
            AlertActionPlan::Restart { .. }
        ));
    }

    #[test]
    fn plans_slack_discord_email_as_actions() {
        let slack =
            alert_rule(r#"alert when cpu > 80% then slack "https://hooks.slack.com/services/xxx""#);
        let discord = alert_rule(
            r#"alert when cpu > 80% then discord "https://discord.com/api/webhooks/xxx""#,
        );
        let email = alert_rule(r#"alert when cpu > 80% then email "admin@example.com""#);
        let email_with_subject =
            alert_rule(r#"alert when cpu > 80% then email "admin@example.com" subject "High CPU""#);
        let collector = MockMetricsCollector {
            samples: vec![sample("api", 90.0)],
        };

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let slack_events = rt
            .block_on(evaluate_alert_once(&slack, &collector))
            .expect("slack");
        let discord_events = rt
            .block_on(evaluate_alert_once(&discord, &collector))
            .expect("discord");
        let email_events = rt
            .block_on(evaluate_alert_once(&email, &collector))
            .expect("email");
        let email_subject_events = rt
            .block_on(evaluate_alert_once(&email_with_subject, &collector))
            .expect("email with subject");

        assert!(
            matches!(slack_events[0].action, AlertActionPlan::Slack { .. }),
            "expected Slack action"
        );
        assert!(
            matches!(discord_events[0].action, AlertActionPlan::Discord { .. }),
            "expected Discord action"
        );
        assert!(
            matches!(email_events[0].action, AlertActionPlan::Email { .. }),
            "expected Email action"
        );
        assert!(
            matches!(
                email_subject_events[0].action,
                AlertActionPlan::Email { .. }
            ),
            "expected Email action with custom subject"
        );

        // Verify email with custom subject preserves the subject
        if let AlertActionPlan::Email {
            ref subject,
            executed,
            ..
        } = email_subject_events[0].action
        {
            assert_eq!(subject, "High CPU");
            assert!(!executed, "email should not actually send in tests");
        }
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
