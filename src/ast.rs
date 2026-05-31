use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Query {
    Observe(ObserveQuery),
    Events(EventsQuery),
    Inspect(InspectQuery),
    Analyze(AnalyzeQuery),
    Alert(AlertRule),
    Fields(CollectionTarget),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ObserveQuery {
    pub target: CollectionTarget,
    pub time: Option<TimeSelector>,
    pub filter: Option<Expression>,
    pub pipeline: Vec<PipelineNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EventsQuery {
    pub target: CollectionTarget,
    pub time: Option<TimeSelector>,
    pub filter: Option<Expression>,
    pub pipeline: Vec<PipelineNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InspectQuery {
    pub target: SingularTarget,
    pub at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AnalyzeQuery {
    pub target: AnalysisTarget,
    pub verb: AnalysisVerb,
    pub subject: Option<String>,
    pub time: Option<TimeSelector>,
    pub pipeline: Vec<PipelineNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AlertRule {
    pub condition: Expression,
    pub duration: Option<Duration>,
    pub action: AlertAction,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub enum CollectionTarget {
    Containers,
    Images,
    Networks,
    Volumes,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SingularTarget {
    pub kind: SingularTargetKind,
    pub value: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub enum SingularTargetKind {
    Container,
    Image,
    Network,
    Volume,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum AnalysisTarget {
    Collection(CollectionTarget),
    Singular(SingularTarget),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub enum AnalysisVerb {
    Find,
    Correlate,
    Explain,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum PipelineNode {
    Where(Expression),
    Select(Vec<String>),
    GroupBy(Vec<String>),
    SortBy {
        field: String,
        direction: SortDirection,
    },
    Limit(u64),
    Alert(String),
    If {
        condition: Expression,
        then_branch: Vec<PipelineNode>,
        else_branch: Option<Vec<PipelineNode>>,
    },
    Set {
        field: String,
        value: SetValue,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum SetValue {
    Literal(Value),
    Case {
        when_clauses: Vec<(Expression, Value)>,
        else_value: Option<Value>,
    },
    IfElse {
        condition: Expression,
        then_value: Value,
        else_value: Option<Value>,
    },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub enum SortDirection {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum TimeSelector {
    Last(Duration),
    Range { from: String, to: String },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub struct Duration {
    pub value: u64,
    pub unit: DurationUnit,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub enum DurationUnit {
    Seconds,
    Minutes,
    Hours,
    Days,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum AlertAction {
    Print(String),
    Webhook(String),
    Restart(SingularTarget),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Expression {
    Comparison {
        field: String,
        operator: Operator,
        value: Value,
    },
    In {
        field: String,
        values: Vec<Value>,
    },
    And(Box<Expression>, Box<Expression>),
    Or(Box<Expression>, Box<Expression>),
    Not(Box<Expression>),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub enum Operator {
    Eq,
    NotEq,
    Gt,
    Lt,
    Gte,
    Lte,
    Contains,
    Matches,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Value {
    String(String),
    Identifier(String),
    Integer(i64),
    Float(f64),
    Percentage(f64),
    Boolean(bool),
}
