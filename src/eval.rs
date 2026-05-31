use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde_json::{Number, Value as JsonValue};
use thiserror::Error;

use crate::ast::{BinOp, Expression, Operator, SetValue, Value};

#[derive(Debug, Error)]
pub enum EvalError {
    #[error("unsupported field `{field}`")]
    UnsupportedField { field: String },
    #[error("field `{field}` cannot be compared with operator `{operator}`")]
    InvalidComparison { field: String, operator: String },
    #[error("arithmetic error: {0}")]
    Arithmetic(String),
    #[error("unknown function `{name}`")]
    UnknownFunction { name: String },
    #[error("invalid arguments for `{name}`")]
    InvalidArguments { name: String },
}

/// Evaluate any expression to a JSON value.
pub fn eval_expr(
    fields: &BTreeMap<String, JsonValue>,
    expression: &Expression,
) -> Result<JsonValue, EvalError> {
    match expression {
        Expression::Field(field) => resolve_field(fields, field).map(|cow| cow.into_owned()),

        Expression::Literal(value) => Ok(value_to_json(value)),

        Expression::Arithmetic { left, op, right } => {
            let l = eval_expr(fields, left)?;
            let r = eval_expr(fields, right)?;
            apply_arithmetic(&l, op, &r)
        }

        Expression::Comparison {
            left,
            operator,
            right,
        } => {
            let result = eval_bool_expr(fields, left, operator, right)?;
            Ok(JsonValue::Bool(result))
        }

        Expression::In { expr, values } => {
            let actual = eval_expr(fields, expr)?;
            for v in values {
                if json_value_eq(&actual, v) {
                    return Ok(JsonValue::Bool(true));
                }
            }
            Ok(JsonValue::Bool(false))
        }

        Expression::Between { expr, low, high } => {
            let val = eval_expr(fields, expr)?;
            let l = eval_expr(fields, low)?;
            let h = eval_expr(fields, high)?;
            let vf = json_as_f64(&val);
            let lf = json_as_f64(&l);
            let hf = json_as_f64(&h);
            match (vf, lf, hf) {
                (Some(v), Some(l), Some(h)) => Ok(JsonValue::Bool(v >= l && v <= h)),
                _ => Ok(JsonValue::Bool(false)),
            }
        }

        Expression::IsNull(expr) => {
            let val = eval_expr_opt(fields, expr);
            let is_null = match val {
                None => true,
                Some(JsonValue::Null) => true,
                Some(val) if val.is_string() && val.as_str().unwrap_or("").is_empty() => true,
                _ => false,
            };
            Ok(JsonValue::Bool(is_null))
        }

        Expression::IsNotNull(expr) => {
            let val = eval_expr_opt(fields, expr);
            let is_not_null = match val {
                None => false,
                Some(JsonValue::Null) => false,
                Some(val) if val.is_string() && val.as_str().unwrap_or("").is_empty() => false,
                _ => true,
            };
            Ok(JsonValue::Bool(is_not_null))
        }

        Expression::FnCall { name, args } => {
            let evaluated: Result<Vec<JsonValue>, _> =
                args.iter().map(|a| eval_expr(fields, a)).collect();
            apply_function(name, &evaluated?)
        }

        Expression::And(left, right) => {
            let l = eval_bool(fields, left)?;
            if !l {
                return Ok(JsonValue::Bool(false));
            }
            let r = eval_bool(fields, right)?;
            Ok(JsonValue::Bool(r))
        }

        Expression::Or(left, right) => {
            let l = eval_bool(fields, left)?;
            if l {
                return Ok(JsonValue::Bool(true));
            }
            let r = eval_bool(fields, right)?;
            Ok(JsonValue::Bool(r))
        }

        Expression::Not(expr) => {
            let val = eval_bool(fields, expr)?;
            Ok(JsonValue::Bool(!val))
        }
    }
}

/// Like eval_expr but returns None instead of erroring on missing fields.
fn eval_expr_opt(
    fields: &BTreeMap<String, JsonValue>,
    expression: &Expression,
) -> Option<JsonValue> {
    match expression {
        Expression::Field(field) => fields.get(field).cloned().or_else(|| {
            // Also try label.xxx access
            if let Some(label_key) = field.strip_prefix("label.")
                && let Some(JsonValue::Array(items)) = fields.get("labels")
            {
                for item in items {
                    if let JsonValue::String(entry) = item
                        && let Some(eq_pos) = entry.find('=')
                    {
                        let key = &entry[..eq_pos];
                        let val = &entry[eq_pos + 1..];
                        if key == label_key {
                            return Some(JsonValue::String(val.to_owned()));
                        }
                    }
                }
            }
            None
        }),
        Expression::Literal(value) => Some(value_to_json(value)),
        _ => eval_expr(fields, expression).ok(),
    }
}

/// Evaluate an expression as a boolean (for `where`, `if` contexts).
pub fn eval_bool(
    fields: &BTreeMap<String, JsonValue>,
    expression: &Expression,
) -> Result<bool, EvalError> {
    let val = eval_expr(fields, expression)?;
    Ok(json_is_truthy(&val))
}

/// Legacy wrapper — kept for backward compatibility.
pub fn evaluate_expression(
    fields: &BTreeMap<String, JsonValue>,
    expression: &Expression,
) -> Result<bool, EvalError> {
    eval_bool(fields, expression)
}

fn json_is_truthy(val: &JsonValue) -> bool {
    match val {
        JsonValue::Null => false,
        JsonValue::Bool(b) => *b,
        JsonValue::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        JsonValue::String(s) => !s.is_empty() && s != "0" && s.to_lowercase() != "false",
        JsonValue::Array(a) => !a.is_empty(),
        JsonValue::Object(_) => true,
    }
}

fn eval_bool_expr(
    fields: &BTreeMap<String, JsonValue>,
    left: &Expression,
    operator: &Operator,
    right: &Expression,
) -> Result<bool, EvalError> {
    let l = eval_expr(fields, left)?;
    let r = eval_expr(fields, right)?;

    match operator {
        Operator::Eq => Ok(json_eq(&l, &r)),
        Operator::NotEq => Ok(!json_eq(&l, &r)),
        Operator::Contains => {
            json_contains_val(&l, &r).ok_or_else(|| EvalError::InvalidComparison {
                field: format!("{l:?}"),
                operator: "Contains".to_owned(),
            })
        }
        Operator::Matches => {
            let pattern = match &r {
                JsonValue::String(s) => s.clone(),
                _ => {
                    return Err(EvalError::InvalidComparison {
                        field: format!("{l:?}"),
                        operator: "Matches".to_owned(),
                    });
                }
            };
            let re = regex::Regex::new(&pattern).map_err(|_| EvalError::InvalidComparison {
                field: format!("{l:?}"),
                operator: "Matches (invalid regex)".to_owned(),
            })?;
            match &l {
                JsonValue::String(s) => Ok(re.is_match(s)),
                JsonValue::Array(values) => Ok(values.iter().any(|v| {
                    let s = render_json_cell(v);
                    re.is_match(&s)
                })),
                _ => Err(EvalError::InvalidComparison {
                    field: format!("{l:?}"),
                    operator: "Matches".to_owned(),
                }),
            }
        }
        Operator::Gt | Operator::Lt | Operator::Gte | Operator::Lte => {
            let ln = json_as_f64(&l).ok_or_else(|| EvalError::InvalidComparison {
                field: format!("{l:?}"),
                operator: format!("{operator:?}"),
            })?;
            let rn = json_as_f64(&r).ok_or_else(|| EvalError::InvalidComparison {
                field: format!("{r:?}"),
                operator: format!("{operator:?}"),
            })?;
            Ok(match operator {
                Operator::Gt => ln > rn,
                Operator::Lt => ln < rn,
                Operator::Gte => ln >= rn,
                Operator::Lte => ln <= rn,
                _ => unreachable!(),
            })
        }
    }
}

fn apply_arithmetic(
    left: &JsonValue,
    op: &BinOp,
    right: &JsonValue,
) -> Result<JsonValue, EvalError> {
    let l = json_as_f64(left)
        .ok_or_else(|| EvalError::Arithmetic(format!("left operand is not numeric: {left:?}")))?;
    let r = json_as_f64(right)
        .ok_or_else(|| EvalError::Arithmetic(format!("right operand is not numeric: {right:?}")))?;
    let result = match op {
        BinOp::Add => l + r,
        BinOp::Sub => l - r,
        BinOp::Mul => l * r,
        BinOp::Div => {
            if r == 0.0 {
                return Err(EvalError::Arithmetic("division by zero".to_owned()));
            }
            l / r
        }
        BinOp::Mod => {
            if r == 0.0 {
                return Err(EvalError::Arithmetic("modulo by zero".to_owned()));
            }
            l % r
        }
    };
    Number::from_f64(result)
        .map(JsonValue::Number)
        .ok_or_else(|| EvalError::Arithmetic("result is NaN or infinity".to_owned()))
}

fn apply_function(name: &str, args: &[JsonValue]) -> Result<JsonValue, EvalError> {
    match name {
        "upper" => {
            let s = args.first().and_then(|v| v.as_str()).ok_or_else(|| {
                EvalError::InvalidArguments {
                    name: name.to_owned(),
                }
            })?;
            Ok(JsonValue::String(s.to_uppercase()))
        }
        "lower" => {
            let s = args.first().and_then(|v| v.as_str()).ok_or_else(|| {
                EvalError::InvalidArguments {
                    name: name.to_owned(),
                }
            })?;
            Ok(JsonValue::String(s.to_lowercase()))
        }
        "length" => {
            let s = args.first().and_then(|v| v.as_str()).ok_or_else(|| {
                EvalError::InvalidArguments {
                    name: name.to_owned(),
                }
            })?;
            Ok(JsonValue::Number(Number::from(s.len() as u64)))
        }
        "trim" => {
            let s = args.first().and_then(|v| v.as_str()).ok_or_else(|| {
                EvalError::InvalidArguments {
                    name: name.to_owned(),
                }
            })?;
            Ok(JsonValue::String(s.trim().to_owned()))
        }
        "concat" => {
            let result: String = args.iter().map(render_json_cell).collect();
            Ok(JsonValue::String(result))
        }
        "substring" => {
            let s = args.first().and_then(|v| v.as_str()).ok_or_else(|| {
                EvalError::InvalidArguments {
                    name: name.to_owned(),
                }
            })?;
            let start = args
                .get(1)
                .and_then(json_as_f64)
                .map(|f| f as usize)
                .unwrap_or(0);
            let len = args
                .get(2)
                .and_then(json_as_f64)
                .map(|f| f as usize)
                .unwrap_or(s.len());
            let end = (start + len).min(s.len());
            let sub = if start < s.len() { &s[start..end] } else { "" };
            Ok(JsonValue::String(sub.to_owned()))
        }
        "coalesce" => {
            for arg in args {
                if !(matches!(arg, JsonValue::Null)
                    || arg.is_string() && arg.as_str().unwrap_or("").is_empty())
                {
                    return Ok(arg.clone());
                }
            }
            Ok(JsonValue::Null)
        }
        _ => Err(EvalError::UnknownFunction {
            name: name.to_owned(),
        }),
    }
}

fn resolve_field<'a>(
    fields: &'a BTreeMap<String, JsonValue>,
    field: &str,
) -> Result<Cow<'a, JsonValue>, EvalError> {
    if let Some(value) = fields.get(field) {
        return Ok(Cow::Borrowed(value));
    }

    if let Some(label_key) = field.strip_prefix("label.") {
        let labels = fields
            .get("labels")
            .ok_or_else(|| EvalError::UnsupportedField {
                field: field.to_owned(),
            })?;

        match labels {
            JsonValue::Array(items) => {
                for item in items {
                    if let JsonValue::String(entry) = item
                        && let Some(eq_pos) = entry.find('=')
                    {
                        let key = &entry[..eq_pos];
                        let val = &entry[eq_pos + 1..];
                        if key == label_key {
                            return Ok(Cow::Owned(JsonValue::String(val.to_owned())));
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

    fields
        .get(field)
        .ok_or_else(|| EvalError::UnsupportedField {
            field: field.to_owned(),
        })
        .map(Cow::Borrowed)
}

pub fn evaluate_set_value(
    fields: &BTreeMap<String, JsonValue>,
    set_value: &SetValue,
) -> Result<JsonValue, EvalError> {
    match set_value {
        SetValue::Literal(value) => Ok(value_to_json(value)),
        SetValue::Expr(expr) => eval_expr(fields, expr),
        SetValue::Case {
            when_clauses,
            else_value,
        } => {
            for (condition, result) in when_clauses {
                if eval_bool(fields, condition)? {
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
            if eval_bool(fields, condition)? {
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
        Value::Float(f) | Value::Percentage(f) => Number::from_f64(*f)
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null),
        Value::Boolean(b) => JsonValue::Bool(*b),
    }
}

pub fn json_eq(left: &JsonValue, right: &JsonValue) -> bool {
    match (left, right) {
        (JsonValue::String(a), JsonValue::String(b)) => a == b,
        (JsonValue::Bool(a), JsonValue::Bool(b)) => a == b,
        (JsonValue::Number(a), _) => a.as_f64() == json_as_f64(right),
        (_, JsonValue::Number(b)) => json_as_f64(left) == b.as_f64(),
        _ => left == right,
    }
}

// Kept for backward compat with the few external callers
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

pub fn json_contains_val(actual: &JsonValue, expected: &JsonValue) -> Option<bool> {
    let expected_str = match expected {
        JsonValue::String(s) => s.clone(),
        other => other.to_string(),
    };

    match actual {
        JsonValue::String(actual) => Some(actual.contains(&expected_str)),
        JsonValue::Array(values) => Some(values.iter().any(|value| match value {
            JsonValue::String(actual) => actual.contains(&expected_str),
            other => other.to_string().contains(&expected_str),
        })),
        _ => None,
    }
}

// Keep old signature for backward compat
pub fn json_contains(actual: &JsonValue, expected: &Value) -> Option<bool> {
    json_contains_val(actual, &value_to_json(expected))
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
    fn cmp_expr(field: &str, op: Operator, val: Value) -> Expression {
        Expression::Comparison {
            left: Box::new(Expression::Field(field.to_owned())),
            operator: op,
            right: Box::new(Expression::Literal(val)),
        }
    }

    fn in_expr(field: &str, values: Vec<Value>) -> Expression {
        Expression::In {
            expr: Box::new(Expression::Field(field.to_owned())),
            values,
        }
    }
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
        let expr = cmp_expr("status", Operator::Eq, Value::String("running".into()));
        assert!(evaluate_expression(&f, &expr).unwrap());
    }

    #[test]
    fn evaluates_numeric_comparison() {
        let mut f = BTreeMap::new();
        f.insert("cpu".into(), serde_json::json!(87.5));
        let expr = cmp_expr("cpu", Operator::Gt, Value::Percentage(80.0));
        assert!(evaluate_expression(&f, &expr).unwrap());
    }

    #[test]
    fn evaluates_and_or_not() {
        let f = fields(&[("a", "1"), ("b", "2")]);
        let left = cmp_expr("a", Operator::Eq, Value::String("1".into()));
        let right = cmp_expr("b", Operator::Eq, Value::String("9".into()));
        assert!(
            evaluate_expression(
                &f,
                &Expression::Or(Box::new(left.clone()), Box::new(right.clone()))
            )
            .unwrap()
        );
        assert!(
            !evaluate_expression(&f, &Expression::And(Box::new(left), Box::new(right))).unwrap()
        );
    }

    #[test]
    fn evaluates_set_case() {
        let mut f = BTreeMap::new();
        f.insert("cpu".into(), serde_json::json!(87.5));
        let sv = SetValue::Case {
            when_clauses: vec![
                (
                    cmp_expr("cpu", Operator::Gt, Value::Percentage(80.0)),
                    Value::String("critical".into()),
                ),
                (
                    cmp_expr("cpu", Operator::Gt, Value::Percentage(50.0)),
                    Value::String("warning".into()),
                ),
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
            when_clauses: vec![(
                cmp_expr("cpu", Operator::Gt, Value::Percentage(80.0)),
                Value::String("critical".into()),
            )],
            else_value: Some(Value::String("ok".into())),
        };
        let result = evaluate_set_value(&f, &sv).unwrap();
        assert_eq!(result, JsonValue::String("ok".to_string()));
    }

    #[test]
    fn evaluates_set_if_else() {
        let f = fields(&[("state", "running")]);
        let sv = SetValue::IfElse {
            condition: cmp_expr("state", Operator::Eq, Value::Identifier("running".into())),
            then_value: Value::String("up".into()),
            else_value: Some(Value::String("down".into())),
        };
        let result = evaluate_set_value(&f, &sv).unwrap();
        assert_eq!(result, JsonValue::String("up".to_string()));
    }

    #[test]
    fn evaluates_matches_regex() {
        let f = fields(&[("name", "api-service-v2")]);
        let expr = cmp_expr(
            "name",
            Operator::Matches,
            Value::String("^api-.*\\d+$".into()),
        );
        assert!(evaluate_expression(&f, &expr).unwrap());
    }

    #[test]
    fn evaluates_matches_regex_no_match() {
        let f = fields(&[("name", "api-service")]);
        let expr = cmp_expr(
            "name",
            Operator::Matches,
            Value::String("^api-.*\\d+$".into()),
        );
        assert!(!evaluate_expression(&f, &expr).unwrap());
    }

    #[test]
    fn evaluates_in_operator() {
        let f = fields(&[("state", "running")]);
        let expr = in_expr(
            "state",
            vec![
                Value::String("running".into()),
                Value::String("restarting".into()),
            ],
        );
        assert!(evaluate_expression(&f, &expr).unwrap());
    }

    #[test]
    fn evaluates_in_operator_no_match() {
        let f = fields(&[("state", "exited")]);
        let expr = in_expr(
            "state",
            vec![
                Value::String("running".into()),
                Value::String("restarting".into()),
            ],
        );
        assert!(!evaluate_expression(&f, &expr).unwrap());
    }

    #[test]
    fn evaluates_label_field_access() {
        let mut f = BTreeMap::new();
        f.insert("labels".into(), serde_json::json!(["env=prod", "tier=api"]));
        let expr = cmp_expr("label.env", Operator::Eq, Value::String("prod".into()));
        assert!(evaluate_expression(&f, &expr).unwrap());
    }

    #[test]
    fn evaluates_label_field_no_match() {
        let mut f = BTreeMap::new();
        f.insert("labels".into(), serde_json::json!(["env=staging"]));
        let expr = cmp_expr("label.env", Operator::Eq, Value::String("prod".into()));
        assert!(!evaluate_expression(&f, &expr).unwrap());
    }

    // ── New Tier 1 tests ──

    #[test]
    fn evaluates_arithmetic() {
        let mut f = BTreeMap::new();
        f.insert("cpu".into(), serde_json::json!(80.0));
        f.insert("mem".into(), serde_json::json!(200.0));

        // cpu + mem > 250
        let sum = Expression::Arithmetic {
            left: Box::new(Expression::Field("cpu".to_owned())),
            op: BinOp::Add,
            right: Box::new(Expression::Field("mem".to_owned())),
        };
        let cmp = Expression::Comparison {
            left: Box::new(sum),
            operator: Operator::Gt,
            right: Box::new(Expression::Literal(Value::Float(250.0))),
        };
        assert!(eval_bool(&f, &cmp).unwrap());

        // cpu * 2 = 160
        let mul = Expression::Arithmetic {
            left: Box::new(Expression::Field("cpu".to_owned())),
            op: BinOp::Mul,
            right: Box::new(Expression::Literal(Value::Float(2.0))),
        };
        let eq = Expression::Comparison {
            left: Box::new(mul),
            operator: Operator::Eq,
            right: Box::new(Expression::Literal(Value::Float(160.0))),
        };
        assert!(eval_bool(&f, &eq).unwrap());
    }

    #[test]
    fn evaluates_between() {
        let mut f = BTreeMap::new();
        f.insert("cpu".into(), serde_json::json!(65.0));
        let expr = Expression::Between {
            expr: Box::new(Expression::Field("cpu".to_owned())),
            low: Box::new(Expression::Literal(Value::Float(50.0))),
            high: Box::new(Expression::Literal(Value::Float(80.0))),
        };
        assert!(eval_bool(&f, &expr).unwrap());

        f.insert("cpu".into(), serde_json::json!(95.0));
        assert!(!eval_bool(&f, &expr).unwrap());
    }

    #[test]
    fn evaluates_is_null() {
        let mut f = BTreeMap::new();
        f.insert("name".into(), JsonValue::String("web".into()));
        let expr = Expression::IsNull(Box::new(Expression::Field("name".to_owned())));
        assert!(!eval_bool(&f, &expr).unwrap());
        assert!(eval_bool(&BTreeMap::new(), &expr).unwrap());
    }

    #[test]
    fn evaluates_is_not_null() {
        let mut f = BTreeMap::new();
        f.insert("name".into(), JsonValue::String("web".into()));
        let expr = Expression::IsNotNull(Box::new(Expression::Field("name".to_owned())));
        assert!(eval_bool(&f, &expr).unwrap());
        assert!(!eval_bool(&BTreeMap::new(), &expr).unwrap());
    }

    #[test]
    fn evaluates_function_calls() {
        let f = fields(&[("name", "Api-Service")]);
        let upper = Expression::FnCall {
            name: "upper".to_owned(),
            args: vec![Expression::Field("name".to_owned())],
        };
        assert_eq!(
            eval_expr(&f, &upper).unwrap(),
            JsonValue::String("API-SERVICE".to_owned())
        );

        let lower = Expression::FnCall {
            name: "lower".to_owned(),
            args: vec![Expression::Field("name".to_owned())],
        };
        assert_eq!(
            eval_expr(&f, &lower).unwrap(),
            JsonValue::String("api-service".to_owned())
        );

        let len = Expression::FnCall {
            name: "length".to_owned(),
            args: vec![Expression::Field("name".to_owned())],
        };
        assert_eq!(
            eval_expr(&f, &len).unwrap(),
            JsonValue::Number(Number::from(11))
        );

        let concat = Expression::FnCall {
            name: "concat".to_owned(),
            args: vec![
                Expression::Field("name".to_owned()),
                Expression::Literal(Value::String(":v1".to_owned())),
            ],
        };
        assert_eq!(
            eval_expr(&f, &concat).unwrap(),
            JsonValue::String("Api-Service:v1".to_owned())
        );
    }

    #[test]
    fn evaluates_set_expr() {
        let mut f = BTreeMap::new();
        f.insert("memory".into(), serde_json::json!(1073741824u64));
        let sv = SetValue::Expr(Expression::Arithmetic {
            left: Box::new(Expression::Field("memory".to_owned())),
            op: BinOp::Div,
            right: Box::new(Expression::Literal(Value::Float(1073741824.0))),
        });
        let result = evaluate_set_value(&f, &sv).unwrap();
        assert_eq!(result, JsonValue::Number(Number::from_f64(1.0).unwrap()));
    }

    #[test]
    fn multiplies_arithmetic() {
        let mut f = BTreeMap::new();
        f.insert("cpu".into(), serde_json::json!(50.0));
        // (cpu + 10) * 2
        let add = Expression::Arithmetic {
            left: Box::new(Expression::Field("cpu".to_owned())),
            op: BinOp::Add,
            right: Box::new(Expression::Literal(Value::Float(10.0))),
        };
        let mul = Expression::Arithmetic {
            left: Box::new(add),
            op: BinOp::Mul,
            right: Box::new(Expression::Literal(Value::Float(2.0))),
        };
        let result = eval_expr(&f, &mul).unwrap();
        assert_eq!(result.as_f64(), Some(120.0));
    }

    #[test]
    fn handles_division_by_zero() {
        let mut f = BTreeMap::new();
        f.insert("x".into(), serde_json::json!(10.0));
        let div = Expression::Arithmetic {
            left: Box::new(Expression::Field("x".to_owned())),
            op: BinOp::Div,
            right: Box::new(Expression::Literal(Value::Float(0.0))),
        };
        assert!(eval_expr(&f, &div).is_err());
    }
}
