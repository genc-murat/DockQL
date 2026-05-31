use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde_json::{Number, Value as JsonValue};
use thiserror::Error;

use crate::ast::{Expression, Operator, SetValue, Value};

#[derive(Debug, Error)]
pub enum EvalError {
    #[error("unsupported field `{field}`")]
    UnsupportedField { field: String },
    #[error("field `{field}` cannot be compared with operator `{operator}`")]
    InvalidComparison { field: String, operator: String },
}

pub fn evaluate_expression(
    fields: &BTreeMap<String, JsonValue>,
    expression: &Expression,
) -> Result<bool, EvalError> {
    match expression {
        Expression::Comparison {
            field,
            operator,
            value,
        } => evaluate_comparison(fields, field, *operator, value),
        Expression::In { field, values } => evaluate_in(fields, field, values),
        Expression::And(left, right) => {
            Ok(evaluate_expression(fields, left)? && evaluate_expression(fields, right)?)
        }
        Expression::Or(left, right) => {
            Ok(evaluate_expression(fields, left)? || evaluate_expression(fields, right)?)
        }
        Expression::Not(expr) => Ok(!evaluate_expression(fields, expr)?),
    }
}

fn resolve_field<'a>(
    fields: &'a BTreeMap<String, JsonValue>,
    field: &str,
) -> Result<Cow<'a, JsonValue>, EvalError> {
    use std::borrow::Cow;

    if let Some(value) = fields.get(field) {
        return Ok(Cow::Borrowed(value));
    }

    if let Some(label_key) = field.strip_prefix("label.") {
        let labels = fields.get("labels").ok_or_else(|| EvalError::UnsupportedField {
            field: field.to_owned(),
        })?;

        match labels {
            JsonValue::Array(items) => {
                for item in items {
                    if let JsonValue::String(entry) = item {
                        if let Some(eq_pos) = entry.find('=') {
                            let key = &entry[..eq_pos];
                            let val = &entry[eq_pos + 1..];
                            if key == label_key {
                                return Ok(Cow::Owned(JsonValue::String(val.to_owned())));
                            }
                        }
                    }
                }
            }
            JsonValue::String(s) => {
                for entry in s.split(',') {
                    let entry = entry.trim();
                    if let Some(eq_pos) = entry.find('=') {
                        let key = &entry[..eq_pos];
                        let val = &entry[eq_pos + 1..];
                        if key == label_key {
                            return Ok(Cow::Owned(JsonValue::String(val.to_owned())));
                        }
                    }
                }
            }
            _ => {}
        }

        return Err(EvalError::UnsupportedField {
            field: field.to_owned(),
        });
    }

    fields.get(field).ok_or_else(|| EvalError::UnsupportedField {
        field: field.to_owned(),
    }).map(Cow::Borrowed)
}

fn evaluate_comparison(
    fields: &BTreeMap<String, JsonValue>,
    field: &str,
    operator: Operator,
    expected: &Value,
) -> Result<bool, EvalError> {
    let actual = resolve_field(fields, field)?;

    match operator {
        Operator::Eq => Ok(json_value_eq(&actual, expected)),
        Operator::NotEq => Ok(!json_value_eq(&actual, expected)),
        Operator::Contains => {
            json_contains(&actual, expected).ok_or_else(|| EvalError::InvalidComparison {
                field: field.to_owned(),
                operator: "Contains".to_owned(),
            })
        }
        Operator::Matches => {
            let pattern = value_as_text(expected).ok_or_else(|| EvalError::InvalidComparison {
                field: field.to_owned(),
                operator: "Matches".to_owned(),
            })?;
            let re = regex::Regex::new(&pattern).map_err(|_| EvalError::InvalidComparison {
                field: field.to_owned(),
                operator: "Matches (invalid regex)".to_owned(),
            })?;
            match &*actual {
                JsonValue::String(s) => Ok(re.is_match(s)),
                JsonValue::Array(values) => Ok(values.iter().any(|v| {
                    let s = render_json_cell(v);
                    re.is_match(&s)
                })),
                _ => Err(EvalError::InvalidComparison {
                    field: field.to_owned(),
                    operator: "Matches".to_owned(),
                }),
            }
        }
        Operator::Gt | Operator::Lt | Operator::Gte | Operator::Lte => {
            let actual_num =
                json_as_f64(&actual).ok_or_else(|| EvalError::InvalidComparison {
                    field: field.to_owned(),
                    operator: format!("{operator:?}"),
                })?;
            let expected_num =
                value_as_f64(expected).ok_or_else(|| EvalError::InvalidComparison {
                    field: field.to_owned(),
                    operator: format!("{operator:?}"),
                })?;
            Ok(match operator {
                Operator::Gt => actual_num > expected_num,
                Operator::Lt => actual_num < expected_num,
                Operator::Gte => actual_num >= expected_num,
                Operator::Lte => actual_num <= expected_num,
                _ => unreachable!("operator handled by outer match"),
            })
        }
    }
}

fn evaluate_in(
    fields: &BTreeMap<String, JsonValue>,
    field: &str,
    values: &[Value],
) -> Result<bool, EvalError> {
    let actual = resolve_field(fields, field)?;

    for expected in values {
        if json_value_eq(&actual, expected) {
            return Ok(true);
        }
    }

    Ok(false)
}

pub fn evaluate_set_value(
    fields: &BTreeMap<String, JsonValue>,
    set_value: &SetValue,
) -> Result<JsonValue, EvalError> {
    match set_value {
        SetValue::Literal(value) => Ok(value_to_json(value)),
        SetValue::Case {
            when_clauses,
            else_value,
        } => {
            for (condition, result) in when_clauses {
                if evaluate_expression(fields, condition)? {
                    return Ok(value_to_json(result));
                }
            }
            Ok(else_value
                .as_ref()
                .map(value_to_json)
                .unwrap_or(JsonValue::Null))
        }
        SetValue::IfElse {
            condition,
            then_value,
            else_value,
        } => {
            if evaluate_expression(fields, condition)? {
                Ok(value_to_json(then_value))
            } else {
                Ok(else_value
                    .as_ref()
                    .map(value_to_json)
                    .unwrap_or(JsonValue::Null))
            }
        }
    }
}

pub fn value_to_json(value: &Value) -> JsonValue {
    match value {
        Value::String(s) => JsonValue::String(s.clone()),
        Value::Identifier(s) => JsonValue::String(s.clone()),
        Value::Integer(n) => JsonValue::Number(Number::from(*n)),
        Value::Float(f) | Value::Percentage(f) => {
            Number::from_f64(*f).map(JsonValue::Number).unwrap_or(JsonValue::Null)
        }
        Value::Boolean(b) => JsonValue::Bool(*b),
    }
}

pub fn json_value_eq(actual: &JsonValue, expected: &Value) -> bool {
    match (actual, expected) {
        (JsonValue::String(actual), Value::String(expected) | Value::Identifier(expected)) => {
            actual == expected
        }
        (JsonValue::Bool(actual), Value::Boolean(expected)) => actual == expected,
        (JsonValue::Number(actual), expected) => actual.as_f64() == value_as_f64(expected),
        _ => false,
    }
}

pub fn json_contains(actual: &JsonValue, expected: &Value) -> Option<bool> {
    let expected = value_as_text(expected)?;

    match actual {
        JsonValue::String(actual) => Some(actual.contains(&expected)),
        JsonValue::Array(values) => Some(values.iter().any(|value| match value {
            JsonValue::String(actual) => actual.contains(&expected),
            other => other.to_string().contains(&expected),
        })),
        _ => None,
    }
}

pub fn compare_json_values(left: &JsonValue, right: &JsonValue) -> Ordering {
    match (json_as_f64(left), json_as_f64(right)) {
        (Some(left), Some(right)) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
        _ => render_json_cell(left).cmp(&render_json_cell(right)),
    }
}

pub fn json_as_f64(value: &JsonValue) -> Option<f64> {
    match value {
        JsonValue::Number(value) => value.as_f64(),
        JsonValue::String(value) => value.parse().ok(),
        _ => None,
    }
}

pub fn value_as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(value) => Some(*value as f64),
        Value::Float(value) | Value::Percentage(value) => Some(*value),
        Value::String(value) | Value::Identifier(value) => value.parse().ok(),
        Value::Boolean(_) => None,
    }
}

pub fn value_as_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) | Value::Identifier(value) => Some(value.clone()),
        Value::Integer(value) => Some(value.to_string()),
        Value::Float(value) | Value::Percentage(value) => Some(value.to_string()),
        Value::Boolean(value) => Some(value.to_string()),
    }
}

pub fn render_json_cell(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => String::new(),
        JsonValue::String(value) => value.clone(),
        JsonValue::Array(values) => values
            .iter()
            .map(render_json_cell)
            .collect::<Vec<_>>()
            .join(","),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, JsonValue> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), JsonValue::String(v.to_string())))
            .collect()
    }

    #[test]
    fn evaluates_string_equality() {
        let f = fields(&[("status", "running")]);
        let expr = Expression::Comparison {
            field: "status".into(),
            operator: Operator::Eq,
            value: Value::String("running".into()),
        };
        assert!(evaluate_expression(&f, &expr).unwrap());
    }

    #[test]
    fn evaluates_numeric_comparison() {
        let mut f = BTreeMap::new();
        f.insert("cpu".into(), serde_json::json!(87.5));
        let expr = Expression::Comparison {
            field: "cpu".into(),
            operator: Operator::Gt,
            value: Value::Percentage(80.0),
        };
        assert!(evaluate_expression(&f, &expr).unwrap());
    }

    #[test]
    fn evaluates_and_or_not() {
        let f = fields(&[("a", "1"), ("b", "2")]);
        let left = Expression::Comparison {
            field: "a".into(),
            operator: Operator::Eq,
            value: Value::String("1".into()),
        };
        let right = Expression::Comparison {
            field: "b".into(),
            operator: Operator::Eq,
            value: Value::String("9".into()),
        };
        assert!(evaluate_expression(&f, &Expression::Or(Box::new(left.clone()), Box::new(right.clone()))).unwrap());
        assert!(!evaluate_expression(&f, &Expression::And(Box::new(left), Box::new(right))).unwrap());
    }

    #[test]
    fn evaluates_set_case() {
        let mut f = BTreeMap::new();
        f.insert("cpu".into(), serde_json::json!(87.5));
        let sv = SetValue::Case {
            when_clauses: vec![
                (Expression::Comparison {
                    field: "cpu".into(),
                    operator: Operator::Gt,
                    value: Value::Percentage(80.0),
                }, Value::String("critical".into())),
                (Expression::Comparison {
                    field: "cpu".into(),
                    operator: Operator::Gt,
                    value: Value::Percentage(50.0),
                }, Value::String("warning".into())),
            ],
            else_value: Some(Value::String("ok".into())),
        };
        let result = evaluate_set_value(&f, &sv).unwrap();
        assert_eq!(result, JsonValue::String("critical".to_string()));
    }

    #[test]
    fn evaluates_set_case_else() {
        let mut f = BTreeMap::new();
        f.insert("cpu".into(), serde_json::json!(30.0));
        let sv = SetValue::Case {
            when_clauses: vec![
                (Expression::Comparison {
                    field: "cpu".into(),
                    operator: Operator::Gt,
                    value: Value::Percentage(80.0),
                }, Value::String("critical".into())),
            ],
            else_value: Some(Value::String("ok".into())),
        };
        let result = evaluate_set_value(&f, &sv).unwrap();
        assert_eq!(result, JsonValue::String("ok".to_string()));
    }

    #[test]
    fn evaluates_set_if_else() {
        let f = fields(&[("state", "running")]);
        let sv = SetValue::IfElse {
            condition: Expression::Comparison {
                field: "state".into(),
                operator: Operator::Eq,
                value: Value::Identifier("running".into()),
            },
            then_value: Value::String("up".into()),
            else_value: Some(Value::String("down".into())),
        };
        let result = evaluate_set_value(&f, &sv).unwrap();
        assert_eq!(result, JsonValue::String("up".to_string()));
    }

    #[test]
    fn evaluates_matches_regex() {
        let f = fields(&[("name", "api-service-v2")]);
        let expr = Expression::Comparison {
            field: "name".into(),
            operator: Operator::Matches,
            value: Value::String("^api-.*\\d+$".into()),
        };
        assert!(evaluate_expression(&f, &expr).unwrap());
    }

    #[test]
    fn evaluates_matches_regex_no_match() {
        let f = fields(&[("name", "api-service")]);
        let expr = Expression::Comparison {
            field: "name".into(),
            operator: Operator::Matches,
            value: Value::String("^api-.*\\d+$".into()),
        };
        assert!(!evaluate_expression(&f, &expr).unwrap());
    }

    #[test]
    fn evaluates_in_operator() {
        let f = fields(&[("state", "running")]);
        let expr = Expression::In {
            field: "state".into(),
            values: vec![
                Value::String("running".into()),
                Value::String("restarting".into()),
            ],
        };
        assert!(evaluate_expression(&f, &expr).unwrap());
    }

    #[test]
    fn evaluates_in_operator_no_match() {
        let f = fields(&[("state", "exited")]);
        let expr = Expression::In {
            field: "state".into(),
            values: vec![
                Value::String("running".into()),
                Value::String("restarting".into()),
            ],
        };
        assert!(!evaluate_expression(&f, &expr).unwrap());
    }

    #[test]
    fn evaluates_label_field_access() {
        let mut f = BTreeMap::new();
        f.insert("labels".into(), serde_json::json!(["env=prod", "tier=api"]));
        let expr = Expression::Comparison {
            field: "label.env".into(),
            operator: Operator::Eq,
            value: Value::String("prod".into()),
        };
        assert!(evaluate_expression(&f, &expr).unwrap());
    }

    #[test]
    fn evaluates_label_field_no_match() {
        let mut f = BTreeMap::new();
        f.insert("labels".into(), serde_json::json!(["env=staging"]));
        let expr = Expression::Comparison {
            field: "label.env".into(),
            operator: Operator::Eq,
            value: Value::String("prod".into()),
        };
        assert!(!evaluate_expression(&f, &expr).unwrap());
    }
}
