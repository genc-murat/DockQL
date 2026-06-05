//! Logical query plan generation.
//!
//! Converts a parsed [`Query`] into a [`LogicalPlan`] for display and
//! optimisation. Plans can be shown via `dol --explain "observe containers"`.
//! Includes a simple filter push-down optimisation.
//!
//! # Example
//!
//! ```ignore
//! let plan = planner::plan(&query);
//! println!("{plan}"); // Shows execution plan
//! ```

use std::fmt;

use crate::ast::{
    AlertRule, AnalyzeQuery, CollectionTarget, EventsQuery, InspectQuery, LogsQuery, ObserveQuery,
    PipelineNode, Query, SortDirection,
};

#[derive(Debug, Clone, PartialEq)]
pub enum LogicalPlan {
    Observe(ObservePlan),
    Events(EventsPlan),
    Inspect(InspectPlan),
    Logs(LogsPlan),
    Compose(ComposePlan),
    Ping,
    Analyze(AnalyzePlan),
    Alert(AlertPlan),
    Fields(crate::ast::CollectionTarget),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObservePlan {
    pub target: CollectionTarget,
    pub join: Option<crate::ast::JoinClause>,
    pub steps: Vec<PlanStep>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventsPlan {
    pub target: CollectionTarget,
    pub steps: Vec<PlanStep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectPlan {
    pub target: crate::ast::SingularTarget,
    pub at: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnalyzePlan {
    pub target: crate::ast::AnalysisTarget,
    pub verb: crate::ast::AnalysisVerb,
    pub subject: Option<String>,
    pub steps: Vec<PlanStep>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlertPlan {
    pub rule: AlertRule,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComposePlan {
    pub project: String,
    pub target: crate::ast::ComposeTarget,
    pub steps: Vec<PlanStep>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogsPlan {
    pub container: String,
    pub tail: Option<u64>,
    pub steps: Vec<PlanStep>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlanStep {
    Fetch(CollectionTarget),
    Filter(crate::ast::Expression),
    In {
        field: String,
        values: Vec<crate::ast::Value>,
    },
    Select(Vec<String>),
    GroupBy(Vec<String>),
    SortBy {
        field: String,
        direction: SortDirection,
    },
    Limit(u64),
    Alert(String),
    If {
        condition: crate::ast::Expression,
        then_branch: Vec<Self>,
        else_branch: Option<Vec<Self>>,
    },
    Set {
        field: String,
        value: crate::ast::SetValue,
    },
}

impl fmt::Display for LogicalPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Observe(p) => {
                writeln!(f, "ObservePlan {{ target: {:?} }}", p.target)?;
                if let Some(join) = &p.join {
                    writeln!(f, "  JOIN {:?} ON ...", join.right)?;
                }
                for step in &p.steps {
                    writeln!(f, "  {step}")?;
                }
                Ok(())
            }
            Self::Events(p) => {
                writeln!(f, "EventsPlan {{ target: {:?} }}", p.target)?;
                for step in &p.steps {
                    writeln!(f, "  {step}")?;
                }
                Ok(())
            }
            Self::Inspect(p) => {
                write!(f, "InspectPlan {{ target: {:?}, at: ", p.target.kind)?;
                match &p.at {
                    Some(t) => write!(f, "Some(\"{t}\")")?,
                    None => write!(f, "None")?,
                }
                writeln!(f, " }}")
            }
            Self::Analyze(p) => {
                writeln!(
                    f,
                    "AnalyzePlan {{ verb: {:?}, subject: {:?} }}",
                    p.verb, p.subject
                )?;
                for step in &p.steps {
                    writeln!(f, "  {step}")?;
                }
                Ok(())
            }
            Self::Alert(p) => {
                writeln!(f, "AlertPlan {{ condition: {:?} }}", p.rule.condition)
            }
            Self::Logs(p) => {
                writeln!(
                    f,
                    "LogsPlan {{ container: {}, tail: {:?} }}",
                    p.container, p.tail
                )?;
                for step in &p.steps {
                    writeln!(f, "  {step}")?;
                }
                Ok(())
            }
            Self::Compose(p) => {
                writeln!(
                    f,
                    "ComposePlan {{ project: {}, target: {:?} }}",
                    p.project, p.target
                )?;
                for step in &p.steps {
                    writeln!(f, "  {step}")?;
                }
                Ok(())
            }
            Self::Ping => {
                writeln!(f, "Ping {{ test Docker connectivity }}")
            }
            Self::Fields(target) => {
                writeln!(f, "FieldsPlan {{ target: {target:?} }}")
            }
        }
    }
}

impl fmt::Display for PlanStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fetch(target) => write!(f, "Fetch({target:?})"),
            Self::Filter(_) => write!(f, "Filter(<expression>)"),
            Self::In { field, values } => {
                write!(f, "In({field}, [")?;
                let vals: Vec<String> = values.iter().map(|v| format!("{v:?}")).collect();
                write!(f, "{}", vals.join(", "))?;
                write!(f, "])")
            }
            Self::Select(fields) => write!(f, "Select({})", fields.join(", ")),
            Self::GroupBy(fields) => write!(f, "GroupBy({})", fields.join(", ")),
            Self::SortBy { field, direction } => write!(f, "SortBy({field}, {direction:?})"),
            Self::Limit(n) => write!(f, "Limit({n})"),
            Self::Alert(msg) => write!(f, "Alert(\"{msg}\")"),
            Self::If {
                condition,
                then_branch,
                else_branch,
            } => {
                write!(f, "If({condition:?}, then=[")?;
                for (i, step) in then_branch.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{step}")?;
                }
                write!(f, "]")?;
                if let Some(else_b) = else_branch {
                    write!(f, ", else=[")?;
                    for (i, step) in else_b.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{step}")?;
                    }
                    write!(f, "]")?;
                }
                write!(f, ")")
            }
            Self::Set { field, value: _ } => write!(f, "Set({field}, <value>)"),
        }
    }
}

#[must_use]
pub fn plan(query: &Query) -> LogicalPlan {
    match query {
        Query::Observe(q) => plan_observe(q),
        Query::Events(q) => plan_events(q),
        Query::Inspect(q) => plan_inspect(q),
        Query::Analyze(q) => plan_analyze(q),
        Query::Alert(rule) => LogicalPlan::Alert(AlertPlan { rule: rule.clone() }),
        Query::Compose(q) => plan_compose(q),
        Query::Logs(q) => plan_logs(q),
        Query::Ping => LogicalPlan::Ping,
        Query::Fields(target) => LogicalPlan::Fields(*target),
    }
}

fn plan_observe(query: &ObserveQuery) -> LogicalPlan {
    let mut steps = vec![PlanStep::Fetch(query.target)];

    if let Some(filter) = &query.filter {
        steps.push(PlanStep::Filter(filter.clone()));
    }

    for node in &query.pipeline {
        steps.push(node_to_step(node));
    }

    optimize_steps(&mut steps);

    LogicalPlan::Observe(ObservePlan {
        target: query.target,
        join: query.join.clone(),
        steps,
    })
}

fn plan_events(query: &EventsQuery) -> LogicalPlan {
    let mut steps = vec![PlanStep::Fetch(query.target)];

    if let Some(filter) = &query.filter {
        steps.push(PlanStep::Filter(filter.clone()));
    }

    for node in &query.pipeline {
        steps.push(node_to_step(node));
    }

    LogicalPlan::Events(EventsPlan {
        target: query.target,
        steps,
    })
}

fn plan_inspect(query: &InspectQuery) -> LogicalPlan {
    LogicalPlan::Inspect(InspectPlan {
        target: query.target.clone(),
        at: query.at.clone(),
    })
}

fn plan_logs(query: &LogsQuery) -> LogicalPlan {
    let mut steps = Vec::new();

    if let Some(filter) = &query.filter {
        steps.push(PlanStep::Filter(filter.clone()));
    }

    for node in &query.pipeline {
        steps.push(node_to_step(node));
    }

    LogicalPlan::Logs(LogsPlan {
        container: query.container.clone(),
        tail: query.tail,
        steps,
    })
}

fn plan_compose(query: &crate::ast::ComposeQuery) -> LogicalPlan {
    let mut steps = Vec::new();

    for node in &query.pipeline {
        steps.push(node_to_step(node));
    }

    LogicalPlan::Compose(ComposePlan {
        project: query.project.clone(),
        target: query.target,
        steps,
    })
}

fn plan_analyze(query: &AnalyzeQuery) -> LogicalPlan {
    let mut steps = Vec::new();
    for node in &query.pipeline {
        steps.push(node_to_step(node));
    }

    LogicalPlan::Analyze(AnalyzePlan {
        target: query.target.clone(),
        verb: query.verb,
        subject: query.subject.clone(),
        steps,
    })
}

fn node_to_step(node: &PipelineNode) -> PlanStep {
    match node {
        PipelineNode::Where(expr) => match expr {
            crate::ast::Expression::In {
                expr: inner_expr,
                values,
            } => {
                let field = match inner_expr.as_ref() {
                    crate::ast::Expression::Field(f) => f.clone(),
                    _ => return PlanStep::Filter(expr.clone()),
                };
                PlanStep::In {
                    field,
                    values: values.clone(),
                }
            }
            other => PlanStep::Filter(other.clone()),
        },
        PipelineNode::Select(fields) => PlanStep::Select(fields.clone()),
        PipelineNode::GroupBy { fields, .. } => PlanStep::GroupBy(fields.clone()),
        PipelineNode::SortBy { fields } => {
            // Take the first sort field for the plan step (primary sort)
            if let Some((field, direction)) = fields.first() {
                PlanStep::SortBy {
                    field: field.clone(),
                    direction: *direction,
                }
            } else {
                PlanStep::SortBy {
                    field: String::new(),
                    direction: SortDirection::Asc,
                }
            }
        }
        PipelineNode::Limit(n) | PipelineNode::Offset(n) => PlanStep::Limit(*n),
        PipelineNode::Alert(msg) => PlanStep::Alert(msg.clone()),
        PipelineNode::If {
            condition,
            then_branch,
            else_branch,
        } => PlanStep::If {
            condition: condition.clone(),
            then_branch: then_branch.iter().map(node_to_step).collect(),
            else_branch: else_branch
                .as_ref()
                .map(|nodes| nodes.iter().map(node_to_step).collect()),
        },
        PipelineNode::Set { field, value } => PlanStep::Set {
            field: field.clone(),
            value: value.clone(),
        },
        PipelineNode::Fill { field, default } => PlanStep::Set {
            field: field.clone(),
            value: crate::ast::SetValue::Expr(default.clone()),
        },
        PipelineNode::Let { name, value } => PlanStep::Set {
            field: name.clone(),
            value: crate::ast::SetValue::Expr(value.clone()),
        },
        PipelineNode::Having(expr) => PlanStep::Filter(expr.clone()),
        PipelineNode::Distinct => PlanStep::Select(vec!["*".to_owned()]),
    }
}

fn optimize_steps(steps: &mut Vec<PlanStep>) {
    push_filters_early(steps);
}
fn push_filters_early(steps: &mut Vec<PlanStep>) {
    let first_sort = steps
        .iter()
        .position(|s| matches!(s, PlanStep::SortBy { .. }));
    let first_limit = steps.iter().position(|s| matches!(s, PlanStep::Limit(_)));

    let barrier = match (first_sort, first_limit) {
        (Some(s), Some(l)) => s.min(l),
        (Some(s), None) => s,
        (None, Some(l)) => l,
        (None, None) => return,
    };

    let filter_indices: Vec<usize> = (barrier + 1..steps.len())
        .filter(|&i| matches!(steps[i], PlanStep::Filter(_)))
        .collect();

    if filter_indices.is_empty() {
        return;
    }

    let filters: Vec<PlanStep> = filter_indices
        .iter()
        .rev()
        .map(|&i| steps.remove(i))
        .collect();
    for (offset, filter) in filters.into_iter().enumerate() {
        steps.insert(1 + offset, filter);
    }
}

#[cfg(test)]
mod tests {
    use crate::parser;

    use super::*;

    #[test]
    fn plans_observe_query() {
        let Query::Observe(q) = parser::parse("observe containers where state = running")
            .unwrap()
            .query
        else {
            panic!("expected observe");
        };
        let p = plan_observe(&q);
        assert!(matches!(p, LogicalPlan::Observe(_)));
        if let LogicalPlan::Observe(op) = p {
            assert_eq!(op.target, CollectionTarget::Containers);
            assert!(op.steps.iter().any(|s| matches!(s, PlanStep::Fetch(_))));
            assert!(op.steps.iter().any(|s| matches!(s, PlanStep::Filter(_))));
        }
    }

    #[test]
    fn plans_events_query() {
        let Query::Events(q) = parser::parse("events containers where action = \"die\"")
            .unwrap()
            .query
        else {
            panic!("expected events");
        };
        let p = plan_events(&q);
        if let LogicalPlan::Events(ep) = p {
            assert_eq!(ep.target, CollectionTarget::Containers);
        }
    }

    #[test]
    fn plans_inspect_query() {
        let Query::Inspect(q) = parser::parse("inspect container api at \"2026-05-31T02:00:00Z\"")
            .unwrap()
            .query
        else {
            panic!("expected inspect");
        };
        let p = plan_inspect(&q);
        if let LogicalPlan::Inspect(ip) = p {
            assert!(ip.at.is_some());
        }
    }

    #[test]
    fn plans_alert_rule() {
        let Query::Alert(rule) =
            parser::parse("alert when cpu > 85% for 2m then print \"High CPU\"")
                .unwrap()
                .query
        else {
            panic!("expected alert");
        };
        let p = plan(&Query::Alert(rule));
        assert!(matches!(p, LogicalPlan::Alert(_)));
    }

    #[test]
    fn optimize_pushes_filters_before_sort() {
        let mut steps = vec![
            PlanStep::Fetch(CollectionTarget::Containers),
            PlanStep::Select(vec!["name".into()]),
            PlanStep::SortBy {
                field: "cpu".into(),
                direction: SortDirection::Desc,
            },
            PlanStep::Filter(crate::ast::Expression::Comparison {
                left: Box::new(crate::ast::Expression::Field("state".into())),
                operator: crate::ast::Operator::Eq,
                right: Box::new(crate::ast::Expression::Literal(crate::ast::Value::String(
                    "running".into(),
                ))),
            }),
        ];
        push_filters_early(&mut steps);

        assert!(matches!(steps[0], PlanStep::Fetch(_)));
        assert!(matches!(steps[1], PlanStep::Filter(_)));
        assert!(matches!(steps[2], PlanStep::Select(_)));
    }

    #[test]
    fn plans_compose_query() {
        let q = parser::parse("compose myapp").unwrap();
        let p = plan(&q.query);
        if let LogicalPlan::Compose(cp) = p {
            assert_eq!(cp.project, "myapp");
            assert_eq!(cp.target, crate::ast::ComposeTarget::Containers);
            assert!(cp.steps.is_empty());
        } else {
            panic!("expected Compose plan, got {:?}", p);
        }
    }

    #[test]
    fn plans_compose_services_with_pipeline() {
        let q =
            parser::parse("compose myapp services | where cpu > 80% | select name, cpu").unwrap();
        let p = plan(&q.query);
        if let LogicalPlan::Compose(cp) = p {
            assert_eq!(cp.project, "myapp");
            assert_eq!(cp.target, crate::ast::ComposeTarget::Services);
            assert_eq!(cp.steps.len(), 2);
        } else {
            panic!("expected Compose plan, got {:?}", p);
        }
    }

    #[test]
    fn plans_compose_with_sort_limit() {
        let q = parser::parse("compose myapp | sort by name asc | limit 5").unwrap();
        let p = plan(&q.query);
        if let LogicalPlan::Compose(cp) = p {
            assert_eq!(cp.steps.len(), 2);
            assert!(matches!(cp.steps[0], PlanStep::SortBy { .. }));
            assert!(matches!(cp.steps[1], PlanStep::Limit(5)));
        } else {
            panic!("expected Compose plan, got {:?}", p);
        }
    }

    #[test]
    fn compose_plan_display() {
        let q = parser::parse("compose myapp services").unwrap();
        let p = plan(&q.query);
        let s = format!("{p}");
        assert!(s.contains("myapp"));
        assert!(s.contains("Services"));
    }

    #[test]
    fn plans_observe_join_images() {
        let q = parser::parse("observe containers join images on id = id").unwrap();
        let p = plan(&q.query);
        if let LogicalPlan::Observe(op) = p {
            assert!(op.join.is_some());
            let join = op.join.unwrap();
            assert_eq!(join.right, CollectionTarget::Images);
        } else {
            panic!("expected Observe plan, got {:?}", p);
        }
    }

    #[test]
    fn plans_observe_join_without_pipeline() {
        let q = parser::parse("observe containers join networks on name = name").unwrap();
        let p = plan(&q.query);
        if let LogicalPlan::Observe(op) = p {
            assert!(op.join.is_some());
            assert_eq!(op.steps.len(), 1);
            assert!(matches!(
                op.steps[0],
                PlanStep::Fetch(CollectionTarget::Containers)
            ));
        } else {
            panic!("expected Observe plan, got {:?}", p);
        }
    }

    #[test]
    fn observe_join_plan_display() {
        let q = parser::parse("observe containers join images on id = id").unwrap();
        let p = plan(&q.query);
        let s = format!("{p}");
        assert!(s.contains("JOIN"));
        assert!(s.contains("Images"));
    }
}
