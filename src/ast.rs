//! Abstract syntax tree (AST) types for parsed DOL queries.
//!
//! Defines all node types produced by the parser: [`Query`] (top-level),
//! [`Expression`] (conditions), [`PipelineNode`] (transformations), and
//! their supporting enums/structs. All types are [`Serialize`] for
//! debugging and analysis.
//!
//! # Example
//!
//! ```ignore
//! use dol::ast::*;
//! let query = Query::Ping;
//! ```

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Query {
    Observe(ObserveQuery),
    Events(EventsQuery),
    Inspect(InspectQuery),
    Analyze(AnalyzeQuery),
    Alert(AlertRule),
    Fields(CollectionTarget),
    Logs(LogsQuery),
    Compose(ComposeQuery),
    Ping,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ObserveQuery {
    pub target: CollectionTarget,
    pub time: Option<TimeSelector>,
    pub filter: Option<Expression>,
    pub join: Option<JoinClause>,
    pub pipeline: Vec<PipelineNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JoinClause {
    pub right: CollectionTarget,
    pub left_key: Expression,
    pub right_key: Expression,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EventsQuery {
    pub target: CollectionTarget,
    pub time: Option<TimeSelector>,
    pub filter: Option<Expression>,
    pub pipeline: Vec<PipelineNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InspectQuery {
    pub target: SingularTarget,
    pub at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ComposeQuery {
    pub project: String,
    pub target: ComposeTarget,
    pub pipeline: Vec<PipelineNode>,
    pub service: Option<String>,
    pub port_number: Option<u64>,
    pub tail: Option<u64>,
    pub config_target: Option<ConfigTarget>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub enum ComposeTarget {
    Containers,
    Services,
    Networks,
    Volumes,
    Health,
    Projects,
    Images,
    Stats,
    Ps,
    Events,
    Port,
    Config,
    Logs,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub enum ConfigTarget {
    All,
    Services,
    Networks,
    Volumes,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LogsQuery {
    pub container: String,
    pub tail: Option<u64>,
    pub filter: Option<Expression>,
    pub pipeline: Vec<PipelineNode>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    GroupBy {
        fields: Vec<String>,
        aggregates: Vec<AggregateExpr>,
    },
    Having(Expression),
    SortBy {
        fields: Vec<(String, SortDirection)>,
    },
    Limit(u64),
    Offset(u64),
    Distinct,
    Alert(String),
    If {
        condition: Expression,
        then_branch: Vec<Self>,
        else_branch: Option<Vec<Self>>,
    },
    Set {
        field: String,
        value: SetValue,
    },
    Fill {
        field: String,
        /// The default value expression (supports if/case/when like `set`).
        default: SetValue,
        /// Optional condition: only fill rows that match this condition.
        condition: Option<Expression>,
    },
    Let {
        name: String,
        value: Expression,
    },
    /// Print debug info (row count and schema) to stderr.
    /// Does not modify the data stream.
    Debug,
    /// Assert that a condition holds for every row.
    /// If any row fails the condition, the query fails with an error.
    /// Acts as a pass-through when all rows pass.
    Assert(Expression),
    /// Assign a sequential row number (1-based) to each row.
    RowNumber {
        /// The alias/column name for the row number.
        alias: String,
    },
    /// Rank rows by a field value (ties get same rank).
    Rank {
        /// Field to rank by.
        field: String,
        /// The alias/column name for the rank.
        alias: String,
    },
    /// Access the value of a field from the previous row (like SQL LAG).
    Lag {
        /// Field whose value from the previous row to retrieve.
        field: String,
        /// The alias/column name for the lag value.
        alias: String,
        /// How many rows to look back (default 1).
        offset: u64,
    },
    /// Access the value of a field from the next row (like SQL LEAD).
    Lead {
        /// Field whose value from the next row to retrieve.
        field: String,
        /// The alias/column name for the lead value.
        alias: String,
        /// How many rows to look ahead (default 1).
        offset: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AggregateExpr {
    pub function: String,
    pub field: String,
    pub alias: String,
    /// Additional arguments for aggregate functions (e.g., percentile value for `percentile`).
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum SetValue {
    Literal(Value),
    Expr(Expression),
    Case {
        when_clauses: Vec<(Expression, Expression)>,
        else_value: Option<Expression>,
    },
    IfElse {
        condition: Expression,
        then_value: Expression,
        else_value: Option<Expression>,
    },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub enum SortDirection {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum AlertAction {
    Print(String),
    Webhook(String),
    Restart(SingularTarget),
    /// Send a formatted message to a Slack channel via Incoming Webhook.
    Slack(String),
    /// Send a formatted message to a Discord channel via Webhook.
    Discord(String),
    /// Send an email notification via SMTP.
    Email {
        /// Recipient email address.
        to: String,
        /// Email subject line.
        subject: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Expression {
    Field(String),
    Literal(Value),
    Arithmetic {
        left: Box<Self>,
        op: BinOp,
        right: Box<Self>,
    },
    Comparison {
        left: Box<Self>,
        operator: Operator,
        right: Box<Self>,
    },
    In {
        expr: Box<Self>,
        values: Vec<Value>,
    },
    Between {
        expr: Box<Self>,
        low: Box<Self>,
        high: Box<Self>,
    },
    IsNull(Box<Self>),
    IsNotNull(Box<Self>),
    FnCall {
        name: String,
        args: Vec<Self>,
    },
    And(Box<Self>, Box<Self>),
    Or(Box<Self>, Box<Self>),
    Not(Box<Self>),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
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
    StartsWith,
    EndsWith,
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
