use crate::ast::{
    AnalyzeQuery, AlertRule, CollectionTarget, EventsQuery, InspectQuery, ObserveQuery,
    PipelineNode, Query, SortDirection,
};

#[derive(Debug, Clone, PartialEq)]
pub enum LogicalPlan {
    Observe(ObservePlan),
    Events(EventsPlan),
    Inspect(InspectPlan),
    Analyze(AnalyzePlan),
    Alert(AlertPlan),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObservePlan {
    pub target: CollectionTarget,
    pub steps: Vec<PlanStep>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventsPlan {
    pub target: CollectionTarget,
    pub steps: Vec<PlanStep>,
}

#[derive(Debug, Clone, PartialEq)]
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
pub enum PlanStep {
    Fetch(CollectionTarget),
    Filter(crate::ast::Expression),
    Select(Vec<String>),
    GroupBy(Vec<String>),
    SortBy { field: String, direction: SortDirection },
    Limit(u64),
    Alert(String),
    If {
        condition: crate::ast::Expression,
        then_branch: Vec<PlanStep>,
        else_branch: Option<Vec<PlanStep>>,
    },
    Set {
        field: String,
        value: crate::ast::SetValue,
    },
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PlanError {
    #[error("empty pipeline after group by: select is required")]
    EmptyPipelineAfterGroupBy,
    #[error("group by without subsequent aggregation is not yet supported")]
    GroupByWithoutAggregation,
}

pub fn plan(query: &Query) -> Result<LogicalPlan, PlanError> {
    match query {
        Query::Observe(q) => plan_observe(q),
        Query::Events(q) => plan_events(q),
        Query::Inspect(q) => plan_inspect(q),
        Query::Analyze(q) => plan_analyze(q),
        Query::Alert(rule) => Ok(LogicalPlan::Alert(AlertPlan { rule: rule.clone() })),
    }
}

fn plan_observe(query: &ObserveQuery) -> Result<LogicalPlan, PlanError> {
    let mut steps = vec![PlanStep::Fetch(query.target)];

    if let Some(filter) = &query.filter {
        steps.push(PlanStep::Filter(filter.clone()));
    }

    for node in &query.pipeline {
        steps.push(node_to_step(node));
    }

    optimize_steps(&mut steps);

    Ok(LogicalPlan::Observe(ObservePlan {
        target: query.target,
        steps,
    }))
}

fn plan_events(query: &EventsQuery) -> Result<LogicalPlan, PlanError> {
    let mut steps = vec![PlanStep::Fetch(query.target)];

    if let Some(filter) = &query.filter {
        steps.push(PlanStep::Filter(filter.clone()));
    }

    for node in &query.pipeline {
        steps.push(node_to_step(node));
    }

    Ok(LogicalPlan::Events(EventsPlan {
        target: query.target,
        steps,
    }))
}

fn plan_inspect(query: &InspectQuery) -> Result<LogicalPlan, PlanError> {
    Ok(LogicalPlan::Inspect(InspectPlan {
        target: query.target.clone(),
        at: query.at.clone(),
    }))
}

fn plan_analyze(query: &AnalyzeQuery) -> Result<LogicalPlan, PlanError> {
    let mut steps = Vec::new();
    for node in &query.pipeline {
        steps.push(node_to_step(node));
    }

    Ok(LogicalPlan::Analyze(AnalyzePlan {
        target: query.target.clone(),
        verb: query.verb,
        subject: query.subject.clone(),
        steps,
    }))
}

fn node_to_step(node: &PipelineNode) -> PlanStep {
    match node {
        PipelineNode::Where(expr) => PlanStep::Filter(expr.clone()),
        PipelineNode::Select(fields) => PlanStep::Select(fields.clone()),
        PipelineNode::GroupBy(fields) => PlanStep::GroupBy(fields.clone()),
        PipelineNode::SortBy { field, direction } => PlanStep::SortBy {
            field: field.clone(),
            direction: *direction,
        },
        PipelineNode::Limit(n) => PlanStep::Limit(*n),
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
    }
}

fn optimize_steps(steps: &mut Vec<PlanStep>) {
    push_filters_early(steps);
}
fn push_filters_early(steps: &mut Vec<PlanStep>) {
    let first_sort = steps.iter().position(|s| matches!(s, PlanStep::SortBy { .. }));
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

    let filters: Vec<PlanStep> = filter_indices.iter().rev().map(|&i| steps.remove(i)).collect();
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
        let Query::Observe(q) = parser::parse("observe containers where state = running").unwrap().query else {
            panic!("expected observe");
        };
        let p = plan_observe(&q).unwrap();
        assert!(matches!(p, LogicalPlan::Observe(_)));
        if let LogicalPlan::Observe(op) = p {
            assert_eq!(op.target, CollectionTarget::Containers);
            assert!(op.steps.iter().any(|s| matches!(s, PlanStep::Fetch(_))));
            assert!(op.steps.iter().any(|s| matches!(s, PlanStep::Filter(_))));
        }
    }

    #[test]
    fn plans_events_query() {
        let Query::Events(q) = parser::parse("events containers where action = \"die\"").unwrap().query else {
            panic!("expected events");
        };
        let p = plan_events(&q).unwrap();
        if let LogicalPlan::Events(ep) = p {
            assert_eq!(ep.target, CollectionTarget::Containers);
        }
    }

    #[test]
    fn plans_inspect_query() {
        let Query::Inspect(q) = parser::parse("inspect container api at \"2026-05-31T02:00:00Z\"").unwrap().query else {
            panic!("expected inspect");
        };
        let p = plan_inspect(&q).unwrap();
        if let LogicalPlan::Inspect(ip) = p {
            assert!(ip.at.is_some());
        }
    }

    #[test]
    fn plans_alert_rule() {
        let Query::Alert(rule) = parser::parse("alert when cpu > 85% for 2m then print \"High CPU\"").unwrap().query else {
            panic!("expected alert");
        };
        let p = plan(&Query::Alert(rule)).unwrap();
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
                field: "state".into(),
                operator: crate::ast::Operator::Eq,
                value: crate::ast::Value::String("running".into()),
            }),
        ];
        push_filters_early(&mut steps);

        assert!(matches!(steps[0], PlanStep::Fetch(_)));
        assert!(matches!(steps[1], PlanStep::Filter(_)));
        assert!(matches!(steps[2], PlanStep::Select(_)));
    }
}
