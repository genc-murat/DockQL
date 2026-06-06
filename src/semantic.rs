//! Semantic analysis and type checking.
//!
//! Validates DOL queries before execution: checks field existence,
//! type compatibility in comparisons/arithmetic, function name validity,
//! and pipeline node semantics. Uses a [`SemanticAnalyzer`] that tracks
//! the active schema as pipeline nodes transform the data.
//!
//! # Example
//!
//! ```ignore
//! semantic::validate_semantics(&query)?;
//! ```

use crate::ast::{CollectionTarget, Expression, PipelineNode, Query, SetValue, Value};
use crate::eval::EvalError;
use crate::target_alias;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    String,
    Integer,
    Float,
    Boolean,
    Percentage,
    Duration,
    Array,
    Unknown,
}

impl Type {
    #[must_use]
    pub const fn is_numeric(&self) -> bool {
        matches!(
            self,
            Self::Integer | Self::Float | Self::Percentage | Self::Duration
        )
    }

    #[must_use]
    pub fn is_compatible(&self, other: &Self) -> bool {
        if *self == Self::Unknown || *other == Self::Unknown {
            return true;
        }
        if self.is_numeric() && other.is_numeric() {
            return true;
        }
        self == other
    }
}

pub struct SemanticAnalyzer {
    active_schema: BTreeMap<String, Type>,
    target: CollectionTarget,
}

impl SemanticAnalyzer {
    #[must_use]
    pub fn new(target: CollectionTarget) -> Self {
        let mut active_schema = BTreeMap::new();
        for (field, ty) in Self::schema_for_target(target) {
            active_schema.insert(field.to_owned(), ty);
        }

        Self {
            active_schema,
            target,
        }
    }

    pub fn validate_query(&mut self, query: &Query) -> Result<(), EvalError> {
        match query {
            Query::Observe(q) => {
                if let Some(join) = &q.join {
                    let right_schema = Self::schema_for_target(join.right);
                    let left_alias = target_alias(self.target);
                    let right_alias = target_alias(join.right);
                    let original_schema = std::mem::take(&mut self.active_schema);
                    for (field, ty) in original_schema {
                        self.active_schema
                            .insert(format!("{left_alias}.{field}"), ty);
                    }
                    for (field, ty) in right_schema {
                        self.active_schema
                            .insert(format!("{right_alias}.{field}"), ty);
                    }
                }
                if let Some(filter) = &q.filter {
                    self.validate_expression(filter)?;
                }
                for node in &q.pipeline {
                    self.apply_pipeline_node(node)?;
                }
            }
            Query::Events(q) => {
                if let Some(filter) = &q.filter {
                    self.validate_expression(filter)?;
                }
                for node in &q.pipeline {
                    self.apply_pipeline_node(node)?;
                }
            }
            Query::Analyze(q) => {
                for node in &q.pipeline {
                    self.apply_pipeline_node(node)?;
                }
            }
            Query::Alert(rule) => {
                self.validate_expression(&rule.condition)?;
            }
            Query::Logs(q) => {
                if let Some(filter) = &q.filter {
                    self.validate_expression(filter)?;
                }
                for node in &q.pipeline {
                    self.apply_pipeline_node(node)?;
                }
            }
            Query::Compose(q) => {
                match q.target {
                    crate::ast::ComposeTarget::Services
                    | crate::ast::ComposeTarget::Ps
                    | crate::ast::ComposeTarget::Stats => {
                        self.active_schema
                            .insert("service".to_owned(), Type::String);
                    }
                    crate::ast::ComposeTarget::Health => {
                        self.active_schema
                            .insert("service".to_owned(), Type::String);
                        if !self.active_schema.contains_key("health") {
                            self.active_schema.insert("health".to_owned(), Type::String);
                        }
                    }
                    crate::ast::ComposeTarget::Projects => {
                        self.active_schema = BTreeMap::new();
                        self.active_schema
                            .insert("project".to_owned(), Type::String);
                        self.active_schema
                            .insert("containers".to_owned(), Type::Integer);
                        self.active_schema
                            .insert("running".to_owned(), Type::Integer);
                        self.active_schema
                            .insert("stopped".to_owned(), Type::Integer);
                        self.active_schema
                            .insert("networks".to_owned(), Type::Integer);
                        self.active_schema
                            .insert("volumes".to_owned(), Type::Integer);
                        self.active_schema.insert("status".to_owned(), Type::String);
                    }
                    crate::ast::ComposeTarget::Images => {
                        self.active_schema = BTreeMap::new();
                        self.active_schema.insert("id".to_owned(), Type::String);
                        self.active_schema
                            .insert("repository".to_owned(), Type::String);
                        self.active_schema.insert("name".to_owned(), Type::String);
                        self.active_schema.insert("tag".to_owned(), Type::String);
                        self.active_schema.insert("digest".to_owned(), Type::String);
                        self.active_schema.insert("size".to_owned(), Type::String);
                        self.active_schema
                            .insert("service".to_owned(), Type::String);
                    }
                    crate::ast::ComposeTarget::Events => {
                        self.active_schema = BTreeMap::new();
                        self.active_schema.insert("time".to_owned(), Type::String);
                        self.active_schema.insert("action".to_owned(), Type::String);
                        self.active_schema
                            .insert("service".to_owned(), Type::String);
                        self.active_schema
                            .insert("container".to_owned(), Type::String);
                        self.active_schema.insert("image".to_owned(), Type::String);
                    }
                    crate::ast::ComposeTarget::Port => {
                        self.active_schema = BTreeMap::new();
                        self.active_schema
                            .insert("service".to_owned(), Type::String);
                        self.active_schema
                            .insert("container".to_owned(), Type::String);
                        self.active_schema.insert("ports".to_owned(), Type::Array);
                    }
                    crate::ast::ComposeTarget::Config => {
                        self.active_schema = BTreeMap::new();
                        self.active_schema.insert("name".to_owned(), Type::String);
                        self.active_schema.insert("image".to_owned(), Type::String);
                        self.active_schema.insert("state".to_owned(), Type::String);
                        self.active_schema.insert("status".to_owned(), Type::String);
                        self.active_schema.insert("ports".to_owned(), Type::Array);
                        self.active_schema
                            .insert("restart_count".to_owned(), Type::Integer);
                        self.active_schema.insert("health".to_owned(), Type::String);
                        self.active_schema
                            .insert("depends_on".to_owned(), Type::Array);
                        self.active_schema.insert("driver".to_owned(), Type::String);
                        self.active_schema.insert("scope".to_owned(), Type::String);
                        self.active_schema
                            .insert("containers".to_owned(), Type::Integer);
                        self.active_schema
                            .insert("mountpoint".to_owned(), Type::String);
                    }
                    crate::ast::ComposeTarget::Logs => {
                        self.active_schema = BTreeMap::new();
                        self.active_schema.insert("line".to_owned(), Type::Integer);
                        self.active_schema
                            .insert("message".to_owned(), Type::String);
                        self.active_schema
                            .insert("service".to_owned(), Type::String);
                        self.active_schema
                            .insert("container".to_owned(), Type::String);
                    }
                    _ => {}
                }
                for node in &q.pipeline {
                    self.apply_pipeline_node(node)?;
                }
            }
            Query::Ping | Query::Inspect(_) | Query::Fields(_) => {}
        }
        Ok(())
    }

    fn check_field_validity(&self, field: &str) -> Result<(), EvalError> {
        if self.active_schema.contains_key(field) {
            return Ok(());
        }
        if field.starts_prefix_label() {
            // Label lookups require the labels field to exist in base schema
            if self.active_schema.contains_key("labels") {
                return Ok(());
            }
        }
        Err(EvalError::UnsupportedField {
            field: field.to_owned(),
        })
    }

    fn infer_expr_type(&self, expr: &Expression) -> Result<Type, EvalError> {
        match expr {
            Expression::Field(f) => {
                self.check_field_validity(f)?;
                if f.starts_prefix_label() {
                    Ok(Type::String)
                } else {
                    Ok(*self.active_schema.get(f).unwrap_or(&Type::Unknown))
                }
            }
            Expression::Literal(v) => match v {
                Value::String(_) | Value::Identifier(_) => Ok(Type::String),
                Value::Integer(_) => Ok(Type::Integer),
                Value::Float(_) => Ok(Type::Float),
                Value::Percentage(_) => Ok(Type::Percentage),
                Value::Boolean(_) => Ok(Type::Boolean),
            },
            Expression::Arithmetic { left, op, right } => {
                let lt = self.infer_expr_type(left)?;
                let rt = self.infer_expr_type(right)?;
                if !lt.is_numeric() || !rt.is_numeric() {
                    return Err(EvalError::Arithmetic(format!(
                        "invalid arithmetic operator '{op:?}' on types {lt:?} and {rt:?}"
                    )));
                }
                if lt == Type::Float || rt == Type::Float {
                    Ok(Type::Float)
                } else {
                    Ok(Type::Integer)
                }
            }
            Expression::Comparison {
                left,
                operator,
                right,
            } => {
                let lt = self.infer_expr_type(left)?;
                let rt = self.infer_expr_type(right)?;
                if !lt.is_compatible(&rt) {
                    let field_name = match &**left {
                        Expression::Field(f) => f.clone(),
                        _ => "expression".to_owned(),
                    };
                    return Err(EvalError::InvalidComparison {
                        field: field_name,
                        operator: format!("{operator:?}"),
                    });
                }
                Ok(Type::Boolean)
            }
            Expression::In { expr, .. }
            | Expression::IsNull(expr)
            | Expression::IsNotNull(expr)
            | Expression::Between { expr, .. } => {
                self.infer_expr_type(expr)?;
                Ok(Type::Boolean)
            }
            Expression::And(left, right) | Expression::Or(left, right) => {
                let lt = self.infer_expr_type(left)?;
                let rt = self.infer_expr_type(right)?;
                if lt != Type::Boolean && lt != Type::Unknown {
                    return Err(EvalError::InvalidComparison {
                        field: "AND/OR left operand".to_owned(),
                        operator: "logical".to_owned(),
                    });
                }
                if rt != Type::Boolean && rt != Type::Unknown {
                    return Err(EvalError::InvalidComparison {
                        field: "AND/OR right operand".to_owned(),
                        operator: "logical".to_owned(),
                    });
                }
                Ok(Type::Boolean)
            }
            Expression::Not(expr) => {
                let t = self.infer_expr_type(expr)?;
                if t != Type::Boolean && t != Type::Unknown {
                    return Err(EvalError::InvalidComparison {
                        field: "NOT operand".to_owned(),
                        operator: "logical".to_owned(),
                    });
                }
                Ok(Type::Boolean)
            }
            Expression::FnCall { name, args } => {
                // Check if function exists
                match name.as_str() {
                    "upper" | "lower" | "trim" | "length" | "concat" | "substring" | "coalesce"
                    | "to_int" | "to_float" | "to_string" | "starts_with" | "ends_with"
                    | "replace" | "reverse" | "repeat" | "split_part" | "position" | "now"
                    | "date_format" | "date_diff" | "extract" => {
                        for arg in args {
                            self.infer_expr_type(arg)?;
                        }
                        match name.as_str() {
                            "to_float" => Ok(Type::Float),
                            "length" | "position" | "date_diff" | "extract" | "to_int" => {
                                Ok(Type::Integer)
                            }
                            "starts_with" | "ends_with" => Ok(Type::Boolean),
                            _ => Ok(Type::String),
                        }
                    }
                    _ => Err(EvalError::UnknownFunction { name: name.clone() }),
                }
            }
        }
    }

    fn validate_expression(&self, expr: &Expression) -> Result<(), EvalError> {
        self.infer_expr_type(expr)?;
        Ok(())
    }

    /// Infer the type produced by a SetValue expression.
    fn validate_set_value_type(&self, value: &SetValue) -> Result<Type, EvalError> {
        match value {
            SetValue::Literal(v) => match v {
                Value::String(_) | Value::Identifier(_) => Ok(Type::String),
                Value::Integer(_) => Ok(Type::Integer),
                Value::Float(_) => Ok(Type::Float),
                Value::Percentage(_) => Ok(Type::Percentage),
                Value::Boolean(_) => Ok(Type::Boolean),
            },
            SetValue::Expr(expr) => self.infer_expr_type(expr),
            SetValue::Case {
                when_clauses,
                else_value,
            } => {
                for (cond, val) in when_clauses {
                    self.validate_expression(cond)?;
                    self.validate_expression(val)?;
                }
                if let Some(else_expr) = else_value {
                    self.validate_expression(else_expr)?;
                }
                Ok(Type::Unknown)
            }
            SetValue::IfElse {
                condition,
                then_value,
                else_value,
            } => {
                self.validate_expression(condition)?;
                self.validate_expression(then_value)?;
                if let Some(else_expr) = else_value {
                    self.validate_expression(else_expr)?;
                }
                Ok(Type::Unknown)
            }
        }
    }

    fn apply_pipeline_node(&mut self, node: &PipelineNode) -> Result<(), EvalError> {
        match node {
            PipelineNode::Where(expr) | PipelineNode::Having(expr) => {
                self.validate_expression(expr)?;
            }
            PipelineNode::Select(fields) => {
                let mut new_schema = BTreeMap::new();
                for f in fields {
                    self.check_field_validity(f)?;
                    let ty = if f.starts_prefix_label() {
                        Type::String
                    } else {
                        *self.active_schema.get(f).unwrap_or(&Type::Unknown)
                    };
                    new_schema.insert(f.clone(), ty);
                }
                self.active_schema = new_schema;
            }
            PipelineNode::SortBy { fields } => {
                for (f, _) in fields {
                    self.check_field_validity(f)?;
                }
            }
            PipelineNode::Limit(_)
            | PipelineNode::Offset(_)
            | PipelineNode::Distinct
            | PipelineNode::Alert(_)
            | PipelineNode::Debug
            | PipelineNode::Assert(_) => {}
            PipelineNode::GroupBy { fields, aggregates } => {
                let mut new_schema = BTreeMap::new();
                for f in fields {
                    self.check_field_validity(f)?;
                    let ty = *self.active_schema.get(f).unwrap_or(&Type::Unknown);
                    new_schema.insert(f.clone(), ty);
                }
                for agg in aggregates {
                    self.check_field_validity(&agg.field)?;
                    let ty = match agg.function.as_str() {
                        "count" => Type::Integer,
                        "sum" | "avg" | "min" | "max" | "median" | "percentile" => Type::Float,
                        _ => Type::Unknown,
                    };
                    new_schema.insert(agg.alias.clone(), ty);
                }
                self.active_schema = new_schema;
            }
            PipelineNode::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.validate_expression(condition)?;
                let mut then_analyzer = Self {
                    active_schema: self.active_schema.clone(),
                    target: self.target,
                };
                for node in then_branch {
                    then_analyzer.apply_pipeline_node(node)?;
                }
                if let Some(else_b) = else_branch {
                    let mut else_analyzer = Self {
                        active_schema: self.active_schema.clone(),
                        target: self.target,
                    };
                    for node in else_b {
                        else_analyzer.apply_pipeline_node(node)?;
                    }
                }
            }
            PipelineNode::Set { field, value } => {
                let ty = match value {
                    SetValue::Literal(v) => match v {
                        Value::String(_) | Value::Identifier(_) => Type::String,
                        Value::Integer(_) => Type::Integer,
                        Value::Float(_) => Type::Float,
                        Value::Percentage(_) => Type::Percentage,
                        Value::Boolean(_) => Type::Boolean,
                    },
                    SetValue::Expr(expr) => self.infer_expr_type(expr)?,
                    SetValue::Case {
                        when_clauses,
                        else_value,
                    } => {
                        for (cond, val) in when_clauses {
                            self.validate_expression(cond)?;
                            self.validate_expression(val)?;
                        }
                        if let Some(else_expr) = else_value {
                            self.validate_expression(else_expr)?;
                        }
                        Type::Unknown
                    }
                    SetValue::IfElse {
                        condition,
                        then_value,
                        else_value,
                    } => {
                        self.validate_expression(condition)?;
                        self.validate_expression(then_value)?;
                        if let Some(else_expr) = else_value {
                            self.validate_expression(else_expr)?;
                        }
                        Type::Unknown
                    }
                };
                self.active_schema.insert(field.clone(), ty);
            }
            PipelineNode::Fill {
                field,
                default,
                condition,
            } => {
                if let Some(cond) = condition {
                    self.validate_expression(cond)?;
                }
                let ty = self.validate_set_value_type(default)?;
                self.active_schema.insert(field.clone(), ty);
            }
            PipelineNode::RowNumber { alias } => {
                self.active_schema.insert(alias.clone(), Type::Integer);
            }
            PipelineNode::Rank { field, alias } => {
                self.check_field_validity(field)?;
                self.active_schema.insert(alias.clone(), Type::Integer);
            }
            PipelineNode::Lag { field, alias, .. } | PipelineNode::Lead { field, alias, .. } => {
                self.check_field_validity(field)?;
                let ty = *self.active_schema.get(field).unwrap_or(&Type::Unknown);
                self.active_schema.insert(alias.clone(), ty);
            }
            PipelineNode::Let { name, value } => {
                let ty = self.infer_expr_type(value)?;
                self.active_schema.insert(name.clone(), ty);
            }
        }
        Ok(())
    }
}

impl SemanticAnalyzer {
    fn schema_for_target(target: CollectionTarget) -> Vec<(&'static str, Type)> {
        match target {
            CollectionTarget::Containers => vec![
                ("id", Type::String),
                ("name", Type::String),
                ("image", Type::String),
                ("status", Type::String),
                ("state", Type::String),
                ("ports", Type::Array),
                ("labels", Type::Array),
                ("compose_project", Type::String),
                ("created_at", Type::String),
                ("started_at", Type::String),
                ("finished_at", Type::String),
                ("restart_count", Type::Integer),
                ("cpu", Type::Float),
                ("memory", Type::Integer),
                ("memory_limit", Type::Integer),
                ("network_rx", Type::Integer),
                ("network_tx", Type::Integer),
                ("disk_read", Type::Integer),
                ("disk_write", Type::Integer),
                ("health", Type::String),
            ],
            CollectionTarget::Images => vec![
                ("id", Type::String),
                ("repository", Type::String),
                ("name", Type::String),
                ("tag", Type::String),
                ("digest", Type::String),
                ("size", Type::String),
                ("created_at", Type::String),
                ("labels", Type::Array),
            ],
            CollectionTarget::Networks => vec![
                ("id", Type::String),
                ("name", Type::String),
                ("driver", Type::String),
                ("scope", Type::String),
                ("containers", Type::Array),
                ("labels", Type::Array),
            ],
            CollectionTarget::Volumes => vec![
                ("name", Type::String),
                ("driver", Type::String),
                ("mountpoint", Type::String),
                ("scope", Type::String),
                ("labels", Type::Array),
            ],
        }
    }
}

trait StartsPrefixLabel {
    fn starts_prefix_label(&self) -> bool;
}

impl StartsPrefixLabel for str {
    fn starts_prefix_label(&self) -> bool {
        self.starts_with("label.")
    }
}

impl StartsPrefixLabel for String {
    fn starts_prefix_label(&self) -> bool {
        self.as_str().starts_prefix_label()
    }
}

pub fn validate_semantics(query: &Query) -> Result<(), EvalError> {
    let target = match query {
        Query::Observe(q) => q.target,
        Query::Events(q) => q.target,
        Query::Analyze(q) => match &q.target {
            crate::ast::AnalysisTarget::Collection(t) => *t,
            crate::ast::AnalysisTarget::Singular(s) => match s.kind {
                crate::ast::SingularTargetKind::Container => CollectionTarget::Containers,
                crate::ast::SingularTargetKind::Image => CollectionTarget::Images,
                crate::ast::SingularTargetKind::Network => CollectionTarget::Networks,
                crate::ast::SingularTargetKind::Volume => CollectionTarget::Volumes,
            },
        },
        Query::Alert(_) | Query::Logs(_) | Query::Ping => CollectionTarget::Containers,
        Query::Compose(q) => match q.target {
            crate::ast::ComposeTarget::Networks => CollectionTarget::Networks,
            crate::ast::ComposeTarget::Volumes => CollectionTarget::Volumes,
            _ => CollectionTarget::Containers,
        },
        Query::Inspect(_) | Query::Fields(_) => return Ok(()),
    };

    let mut analyzer = SemanticAnalyzer::new(target);
    analyzer.validate_query(query)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser;

    #[test]
    fn test_valid_query() {
        let parsed =
            parser::parse("observe containers | where cpu > 50% | select name, cpu").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_invalid_field() {
        let parsed = parser::parse("observe containers | where invalid_field = 1").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(matches!(res, Err(EvalError::UnsupportedField { .. })));
    }

    #[test]
    fn test_invalid_comparison() {
        let parsed = parser::parse("observe containers | where state > 50").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(matches!(res, Err(EvalError::InvalidComparison { .. })));
    }

    #[test]
    fn test_invalid_arithmetic() {
        let parsed = parser::parse("observe containers | set val = state + 1").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(matches!(res, Err(EvalError::Arithmetic(_))));
    }

    #[test]
    fn test_unknown_function() {
        let parsed =
            parser::parse("observe containers | where invalid_fn(name) = \"test\"").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(matches!(res, Err(EvalError::UnknownFunction { .. })));
    }

    #[test]
    fn test_set_field_inheritance() {
        let parsed =
            parser::parse("observe containers | set tier = \"prod\" | select name, tier").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_label_lookup() {
        let parsed =
            parser::parse("observe containers | where label.env = \"production\"").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_nested_if_branch() {
        let parsed = parser::parse(
            "observe containers | if cpu > 90% then set high = true else set high = false",
        )
        .unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    // ── Comprehensive if-branching tests ──

    #[test]
    fn test_if_without_else() {
        let parsed =
            parser::parse("observe containers | if cpu > 90% then alert \"high\"").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_if_complex_condition() {
        let parsed = parser::parse(
            "observe containers | if cpu > 80% and memory > 500 then alert \"high\" else alert \"low\"",
        )
        .unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_if_function_condition() {
        let parsed =
            parser::parse("observe containers | if upper(name) = \"API\" then set found = true")
                .unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_else_if_chain() {
        let parsed = parser::parse(
            "observe containers | if cpu > 90% then alert \"critical\" else if cpu > 70% then alert \"warning\" else alert \"ok\"",
        )
        .unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_if_with_select_in_branch() {
        let parsed =
            parser::parse("observe containers | if state = running then select name, state, cpu")
                .unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_if_with_sort_limit_in_branch() {
        let parsed = parser::parse(
            "observe containers | if state = running then sort by cpu desc | limit 10",
        )
        .unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_if_with_distinct_in_branch() {
        let parsed =
            parser::parse("observe containers | if state = running then distinct").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_multiple_if_nodes() {
        let parsed = parser::parse(
            "observe containers | if cpu > 90% then alert \"high\" | if memory > 500 then alert \"high_mem\"",
        )
        .unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_if_with_expr_set_in_branch() {
        let parsed = parser::parse(
            "observe containers | if state = running then set val = restart_count + 1 else set val = restart_count",
        )
        .unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_if_with_where_in_branch() {
        let parsed = parser::parse(
            "observe containers | if state = running then where cpu > 80% | select name, cpu",
        )
        .unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_nested_if_inside_then() {
        let parsed = parser::parse(
            "observe containers | if cpu > 90% then if memory > 500 then alert \"high\"",
        )
        .unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_nested_if_inside_else() {
        let parsed = parser::parse(
            "observe containers | if cpu > 90% then set critical = true else if memory > 500 then set high_mem = true",
        )
        .unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    // ── Invalid if-branching tests ──

    #[test]
    fn test_if_invalid_field_in_condition() {
        let parsed =
            parser::parse("observe containers | if nonexistent > 50 then alert \"bad\"").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(matches!(res, Err(EvalError::UnsupportedField { .. })));
    }

    #[test]
    fn test_if_invalid_comparison_in_condition() {
        let parsed =
            parser::parse("observe containers | if state > 50 then alert \"bad\"").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(matches!(res, Err(EvalError::InvalidComparison { .. })));
    }

    #[test]
    fn test_if_set_then_select_in_same_branch() {
        // set + select within the same then branch should work
        let parsed = parser::parse(
            "observe containers | if cpu > 90% then set tier = \"high\" | select name, tier",
        )
        .unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_if_select_nonexistent_in_branch() {
        // select with nonexistent field inside a branch should fail
        let parsed =
            parser::parse("observe containers | if cpu > 90% then select name, nonexistent")
                .unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(matches!(res, Err(EvalError::UnsupportedField { .. })));
    }

    #[test]
    fn test_if_invalid_field_in_branch() {
        // Invalid field in where inside then branch
        let parsed =
            parser::parse("observe containers | if cpu > 90% then where nonexistent = 1").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(matches!(res, Err(EvalError::UnsupportedField { .. })));
    }

    #[test]
    fn test_if_invalid_arithmetic_in_branch_set() {
        // Invalid arithmetic inside then branch (string + integer)
        let parsed =
            parser::parse("observe containers | if cpu > 90% then set val = state + 1").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(matches!(res, Err(EvalError::Arithmetic(_))));
    }

    #[test]
    fn test_if_unknown_function_in_branch_where() {
        // Unknown function inside else branch
        let parsed = parser::parse(
            "observe containers | if cpu > 90% then alert \"high\" else where bogus_fn(name) = \"x\"",
        )
        .unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(matches!(res, Err(EvalError::UnknownFunction { .. })));
    }

    #[test]
    fn test_invalid_and_or_types() {
        let parsed = parser::parse("observe containers | where state and name").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_err());
    }

    #[test]
    fn test_distinct_and_limit() {
        let parsed = parser::parse("observe containers | distinct | limit 5").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_group_by_fields() {
        let parsed =
            parser::parse("observe containers | group by state with count(id) as cnt").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_other_collection_targets() {
        let parsed = parser::parse("observe images | select name, size").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());

        let parsed = parser::parse("observe networks | select name, scope").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());

        let parsed = parser::parse("observe volumes | select name, scope").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_compose_valid_query() {
        let parsed = parser::parse("compose myapp | where cpu > 50% | select name, cpu").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_compose_invalid_field() {
        let parsed = parser::parse("compose myapp | where nonexistent_field = 1").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(matches!(res, Err(EvalError::UnsupportedField { .. })));
    }

    #[test]
    fn test_compose_services_service_field_available() {
        let parsed = parser::parse("compose myapp services | select name, service").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_compose_services_where_service() {
        let parsed = parser::parse("compose myapp services | where service = \"api\"").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_compose_set_and_select() {
        let parsed =
            parser::parse("compose myapp | set tier = \"prod\" | select name, tier").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_compose_group_by() {
        let parsed = parser::parse("compose myapp | group by state with count(id) as cnt").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_compose_invalid_comparison() {
        let parsed = parser::parse("compose myapp | where state > 50").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(matches!(res, Err(EvalError::InvalidComparison { .. })));
    }

    #[test]
    fn test_compose_networks_valid_fields() {
        let parsed = parser::parse("compose myapp networks | select name, driver").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_compose_volumes_valid_fields() {
        let parsed = parser::parse("compose myapp volumes | select name, driver").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_compose_health_service_and_health_fields() {
        let parsed = parser::parse("compose myapp health | select name, service, health").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_compose_networks_invalid_field() {
        let parsed = parser::parse("compose myapp networks | where cpu > 50").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(matches!(res, Err(EvalError::UnsupportedField { .. })));
    }

    #[test]
    fn test_compose_volumes_invalid_field() {
        let parsed = parser::parse("compose myapp volumes | where state = \"running\"").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(matches!(res, Err(EvalError::UnsupportedField { .. })));
    }

    #[test]
    fn test_join_prefixed_fields_valid() {
        let parsed = parser::parse("observe containers join images on id = id | where c.image = \"nginx:latest\" | select c.name, i.name").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn test_join_prefixed_field_rejects_non_prefixed() {
        let parsed =
            parser::parse("observe containers join images on id = id | where name = \"web\"")
                .unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(matches!(res, Err(EvalError::UnsupportedField { .. })));
    }

    #[test]
    fn test_join_where_clause_invalid_field() {
        let parsed =
            parser::parse("observe containers join images on id = id | where nonexistent = \"x\"")
                .unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(matches!(res, Err(EvalError::UnsupportedField { .. })));
    }

    #[test]
    fn test_join_no_pipeline() {
        let parsed = parser::parse("observe containers join images on id = id").unwrap();
        let res = validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }
}
