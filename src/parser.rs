use serde::Serialize;

use crate::ast::{
    AlertAction, AlertRule, AnalysisTarget, AnalysisVerb, BinOp, CollectionTarget, ComposeTarget,
    Duration, DurationUnit, EventsQuery, Expression, InspectQuery, LogsQuery, Operator,
    PipelineNode, Query, SetValue, SingularTarget, SingularTargetKind, SortDirection, TimeSelector,
    Value,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ParsedQuery {
    pub source: String,
    pub query: Query,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ParseError {
    pub column: usize,
    pub message: String,
    /// The original query source (trimmed), used to show surrounding context.
    pub source: Option<String>,
}

impl std::error::Error for ParseError {}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "parse error at column {}: {}", self.column, self.message)?;

        if let Some(ref source) = self.source {
            // Show the source line with a pointer at the error column.
            // The column is 1-indexed; we find the (approximate) line
            // containing that column for multi-line queries.
            let mut remaining = self.column;
            let display_line = source
                .lines()
                .find(|line| {
                    let line_len = line.chars().count();
                    if remaining <= line_len || line_len == 0 && remaining == 1 {
                        true
                    } else {
                        remaining = remaining.saturating_sub(line_len + 1);
                        false
                    }
                })
                .unwrap_or(source);

            let col = remaining.min(display_line.chars().count());
            write!(f, "\n  --> {display_line}")?;
            write!(f, "\n     {}{}", " ".repeat(col.saturating_sub(1)), "^")?;
        }

        Ok(())
    }
}

impl ParseError {
    pub fn new(column: usize, message: impl Into<String>) -> Self {
        Self {
            column,
            message: message.into(),
            source: None,
        }
    }

    pub fn with_source(mut self, source: String) -> Self {
        self.source = Some(source);
        self
    }
}

pub fn parse(source: &str) -> Result<ParsedQuery, ParseError> {
    let trimmed = source.trim();

    let result = (|| {
        if trimmed.is_empty() {
            return Err(ParseError::new(1, "empty DOL query"));
        }

        let tokens = tokenize(trimmed)?;
        let mut parser = Parser::new(tokens, trimmed.chars().count() + 1);
        let query = parser.parse_query()?;
        parser.expect_eof()?;

        Ok(ParsedQuery {
            source: trimmed.to_owned(),
            query,
        })
    })();

    // Attach the trimmed source to all parse errors for context display.
    result.map_err(|e| e.with_source(trimmed.to_owned()))
}

#[derive(Debug, Clone, PartialEq)]
struct Token {
    kind: TokenKind,
    column: usize,
}

#[derive(Debug, Clone, PartialEq)]
enum TokenKind {
    Ident(String),
    String(String),
    Integer(i64),
    Float(f64),
    Percentage(f64),
    Duration(Duration),
    Eq,
    NotEq,
    Gt,
    Lt,
    Gte,
    Lte,
    Pipe,
    Comma,
    LParen,
    RParen,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
}

struct Parser {
    tokens: Vec<Token>,
    cursor: usize,
    eof_column: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>, eof_column: usize) -> Self {
        Self {
            tokens,
            cursor: 0,
            eof_column,
        }
    }

    // ── Query dispatch ──

    fn parse_query(&mut self) -> Result<Query, ParseError> {
        match self.peek_ident() {
            Some("observe") => self.parse_observe(),
            Some("events") => self.parse_events(),
            Some("inspect") => self.parse_inspect(),
            Some("analyze") => self.parse_analyze(),
            Some("alert") => self.parse_alert_rule().map(Query::Alert),
            Some("logs") => self.parse_logs(),
            Some("compose") => self.parse_compose(),
            Some("ping") => {
                self.advance();
                Ok(Query::Ping)
            }
            Some("fields") => self.parse_fields(),
            Some(other) => Err(self.error_here(format!(
                "expected query family, found `{other}`; try `observe containers`"
            ))),
            None => Err(self.error_here("expected query family")),
        }
    }

    fn parse_observe(&mut self) -> Result<Query, ParseError> {
        self.expect_ident("observe")?;
        // Support "observe compose <project>" syntax
        if self.consume_ident("compose") {
            let project = self.expect_identifier_like("compose project name")?;
            let target = self.parse_compose_target();
            let pipeline = self.parse_pipeline()?;
            return Ok(Query::Compose(crate::ast::ComposeQuery {
                project,
                target,
                pipeline,
            }));
        }
        let target = self.parse_collection_target()?;
        let time = self.parse_optional_time_selector()?;
        let filter = self.parse_optional_inline_where()?;
        let join = if self.consume_ident("join") {
            let right = self.parse_collection_target()?;
            self.expect_ident("on")?;
            let left_key = self.parse_arith_expression()?;
            self.expect(TokenKind::Eq, "`=`")?;
            let right_key = self.parse_arith_expression()?;
            Some(crate::ast::JoinClause {
                right,
                left_key,
                right_key,
            })
        } else {
            None
        };
        let pipeline = self.parse_pipeline()?;
        Ok(Query::Observe(crate::ast::ObserveQuery {
            target,
            time,
            filter,
            join,
            pipeline,
        }))
    }

    fn parse_events(&mut self) -> Result<Query, ParseError> {
        self.expect_ident("events")?;
        let target = self.parse_collection_target()?;
        let time = self.parse_optional_time_selector()?;
        let filter = self.parse_optional_inline_where()?;
        let pipeline = self.parse_pipeline()?;
        Ok(Query::Events(EventsQuery {
            target,
            time,
            filter,
            pipeline,
        }))
    }

    fn parse_inspect(&mut self) -> Result<Query, ParseError> {
        self.expect_ident("inspect")?;
        let target = self.parse_singular_target()?;
        let at = if self.consume_ident("at") {
            Some(self.expect_timestamp_string()?)
        } else {
            None
        };
        Ok(Query::Inspect(InspectQuery { target, at }))
    }

    fn parse_analyze(&mut self) -> Result<Query, ParseError> {
        self.expect_ident("analyze")?;
        let target = self.parse_analysis_target()?;
        let verb = self.parse_analysis_verb()?;
        let subject = if self.is_at_time_or_pipe_or_eof() {
            None
        } else {
            Some(self.expect_identifier_like("analysis subject")?)
        };
        let time = self.parse_optional_time_selector()?;
        let pipeline = self.parse_pipeline()?;
        Ok(Query::Analyze(crate::ast::AnalyzeQuery {
            target,
            verb,
            subject,
            time,
            pipeline,
        }))
    }

    fn parse_alert_rule(&mut self) -> Result<AlertRule, ParseError> {
        self.expect_ident("alert")?;
        self.expect_ident("when")?;
        let condition = self.parse_expression()?;
        let duration = if self.consume_ident("for") {
            Some(self.expect_duration()?)
        } else {
            None
        };
        self.expect_ident("then")?;
        let action = self.parse_alert_action()?;
        Ok(AlertRule {
            condition,
            duration,
            action,
        })
    }

    fn parse_compose(&mut self) -> Result<Query, ParseError> {
        self.expect_ident("compose")?;
        let project = self.expect_identifier_like("compose project name")?;
        let target = self.parse_compose_target();
        let pipeline = self.parse_pipeline()?;
        Ok(Query::Compose(crate::ast::ComposeQuery {
            project,
            target,
            pipeline,
        }))
    }

    fn parse_compose_target(&mut self) -> ComposeTarget {
        if self.consume_ident("services") {
            ComposeTarget::Services
        } else if self.consume_ident("networks") {
            ComposeTarget::Networks
        } else if self.consume_ident("volumes") {
            ComposeTarget::Volumes
        } else if self.consume_ident("health") {
            ComposeTarget::Health
        } else {
            self.consume_ident("containers");
            ComposeTarget::Containers
        }
    }

    fn parse_logs(&mut self) -> Result<Query, ParseError> {
        self.expect_ident("logs")?;
        self.expect_ident("container")?;
        let container = self.expect_identifier_like("container name or ID")?;
        let tail = if self.consume_ident("tail") {
            Some(self.expect_u64("tail")?)
        } else {
            None
        };
        let filter = self.parse_optional_inline_where()?;
        let pipeline = self.parse_pipeline()?;
        Ok(Query::Logs(LogsQuery {
            container,
            tail,
            filter,
            pipeline,
        }))
    }

    fn parse_fields(&mut self) -> Result<Query, ParseError> {
        self.expect_ident("fields")?;
        let target = self.parse_collection_target()?;
        Ok(Query::Fields(target))
    }

    // ── Target parsing ──

    fn parse_collection_target(&mut self) -> Result<CollectionTarget, ParseError> {
        match self.peek_ident() {
            Some("containers") => { self.advance(); Ok(CollectionTarget::Containers) }
            Some("images") => { self.advance(); Ok(CollectionTarget::Images) }
            Some("networks") => { self.advance(); Ok(CollectionTarget::Networks) }
            Some("volumes") => { self.advance(); Ok(CollectionTarget::Volumes) }
            Some(other) => Err(self.error_here(format!("expected collection target: containers, images, networks, or volumes, found `{other}`"))),
            None => Err(self.error_here("expected collection target: containers, images, networks, or volumes")),
        }
    }

    fn parse_singular_target(&mut self) -> Result<SingularTarget, ParseError> {
        let kind = match self.peek_ident() {
            Some("container") => {
                self.advance();
                SingularTargetKind::Container
            }
            Some("image") => {
                self.advance();
                SingularTargetKind::Image
            }
            Some("network") => {
                self.advance();
                SingularTargetKind::Network
            }
            Some("volume") => {
                self.advance();
                SingularTargetKind::Volume
            }
            Some(other) => return Err(self.error_here(format!(
                "expected singular target: container, image, network, or volume, found `{other}`"
            ))),
            None => {
                return Err(self
                    .error_here("expected singular target: container, image, network, or volume"));
            }
        };
        let value = self.expect_identifier_like("target name or ID")?;
        Ok(SingularTarget { kind, value })
    }

    fn parse_analysis_target(&mut self) -> Result<AnalysisTarget, ParseError> {
        if let Ok(target) = self.parse_collection_target() {
            return Ok(AnalysisTarget::Collection(target));
        }
        self.parse_singular_target().map(AnalysisTarget::Singular)
    }

    fn parse_analysis_verb(&mut self) -> Result<AnalysisVerb, ParseError> {
        match self.peek_ident() {
            Some("find") => {
                self.advance();
                Ok(AnalysisVerb::Find)
            }
            Some("correlate") => {
                self.advance();
                Ok(AnalysisVerb::Correlate)
            }
            Some("explain") => {
                self.advance();
                Ok(AnalysisVerb::Explain)
            }
            Some(other) => Err(self.error_here(format!(
                "expected analysis verb: find, correlate, or explain, found `{other}`"
            ))),
            None => Err(self.error_here("expected analysis verb: find, correlate, or explain")),
        }
    }

    fn parse_alert_action(&mut self) -> Result<AlertAction, ParseError> {
        match self.peek_ident() {
            Some("print") => {
                self.advance();
                let msg = self.expect_string("alert message")?;
                Ok(AlertAction::Print(msg))
            }
            Some("webhook") => {
                self.advance();
                let url = self.expect_string("webhook URL")?;
                Ok(AlertAction::Webhook(url))
            }
            Some("restart") => {
                self.advance();
                let target = self.parse_singular_target()?;
                Ok(AlertAction::Restart(target))
            }
            Some(other) => Err(self.error_here(format!(
                "expected alert action: print, webhook, or restart, found `{other}`"
            ))),
            None => Err(self.error_here("expected alert action: print, webhook, or restart")),
        }
    }

    // ── Time parsing ──

    fn parse_optional_time_selector(&mut self) -> Result<Option<TimeSelector>, ParseError> {
        if self.consume_ident("last") {
            let duration = self.expect_duration()?;
            Ok(Some(TimeSelector::Last(duration)))
        } else if self.consume_ident("from") {
            let from = self.expect_timestamp_string()?;
            self.expect_ident("to")?;
            let to = self.expect_timestamp_string()?;
            Ok(Some(TimeSelector::Range { from, to }))
        } else {
            Ok(None)
        }
    }

    fn parse_optional_inline_where(&mut self) -> Result<Option<Expression>, ParseError> {
        if self.consume_ident("where") {
            Ok(Some(self.parse_expression()?))
        } else {
            Ok(None)
        }
    }

    // ── Pipeline parsing ──

    fn parse_pipeline(&mut self) -> Result<Vec<PipelineNode>, ParseError> {
        let mut nodes = Vec::new();
        while self.consume(TokenKind::Pipe) {
            nodes.push(self.parse_pipeline_node()?);
        }
        Ok(nodes)
    }

    fn parse_pipeline_node(&mut self) -> Result<PipelineNode, ParseError> {
        if self.consume_ident("where") {
            return Ok(PipelineNode::Where(self.parse_expression()?));
        }
        if self.consume_ident("select") {
            return Ok(PipelineNode::Select(self.parse_field_list()?));
        }
        if self.consume_ident("group") {
            self.expect_ident("by")?;
            let fields = self.parse_field_list()?;
            let aggregates = if self.consume_ident("with") {
                self.parse_aggregates()?
            } else {
                Vec::new()
            };
            return Ok(PipelineNode::GroupBy { fields, aggregates });
        }
        if self.consume_ident("having") {
            return Ok(PipelineNode::Having(self.parse_expression()?));
        }
        if self.consume_ident("sort") {
            self.consume_ident("by");
            let mut fields = Vec::new();
            loop {
                let field = self.expect_identifier_like("sort field")?;
                let direction = if self.consume_ident("desc") {
                    SortDirection::Desc
                } else {
                    self.consume_ident("asc");
                    SortDirection::Asc
                };
                fields.push((field, direction));
                if !self.consume(TokenKind::Comma) {
                    break;
                }
            }
            return Ok(PipelineNode::SortBy { fields });
        }
        if self.consume_ident("limit") {
            return Ok(PipelineNode::Limit(self.expect_u64("limit")?));
        }
        if self.consume_ident("offset") {
            return Ok(PipelineNode::Offset(self.expect_u64("offset")?));
        }
        if self.consume_ident("distinct") {
            return Ok(PipelineNode::Distinct);
        }
        if self.consume_ident("alert") {
            return Ok(PipelineNode::Alert(self.expect_string("alert message")?));
        }
        if self.consume_ident("if") {
            return self.parse_if_pipeline();
        }
        if self.consume_ident("set") {
            return self.parse_set_pipeline();
        }
        if self.consume_ident("fill") {
            return self.parse_fill_pipeline();
        }
        Err(self.error_here("expected pipeline node: where, select, group by, sort by, limit, offset, distinct, alert, if, set, or fill"))
    }

    fn parse_aggregates(&mut self) -> Result<Vec<crate::ast::AggregateExpr>, ParseError> {
        let mut aggs = Vec::new();
        loop {
            let func = self.expect_identifier_like("aggregate function")?;
            self.expect(TokenKind::LParen, "`(`")?;
            let field = self.expect_identifier_like("aggregate field")?;
            self.expect(TokenKind::RParen, "`)`")?;
            let alias = if self.consume_ident("as") {
                self.expect_identifier_like("alias")?
            } else {
                field.clone()
            };
            aggs.push(crate::ast::AggregateExpr {
                function: func,
                field,
                alias,
            });
            if !self.consume(TokenKind::Comma) {
                break;
            }
        }
        Ok(aggs)
    }

    fn parse_if_pipeline(&mut self) -> Result<PipelineNode, ParseError> {
        let condition = self.parse_expression()?;
        self.expect_ident("then")?;
        let then_branch = self.parse_inline_pipeline_nodes()?;
        let else_branch = if self.consume_ident("else") {
            if self.consume_ident("if") {
                Some(vec![self.parse_if_pipeline()?])
            } else {
                Some(self.parse_inline_pipeline_nodes()?)
            }
        } else {
            None
        };
        Ok(PipelineNode::If {
            condition,
            then_branch,
            else_branch,
        })
    }

    fn parse_inline_pipeline_nodes(&mut self) -> Result<Vec<PipelineNode>, ParseError> {
        let mut nodes = vec![self.parse_pipeline_node()?];
        while self.consume(TokenKind::Pipe) {
            nodes.push(self.parse_pipeline_node()?);
        }
        Ok(nodes)
    }

    fn parse_set_pipeline(&mut self) -> Result<PipelineNode, ParseError> {
        let field = self.expect_identifier_like("field name")?;
        self.expect(TokenKind::Eq, "`=`")?;
        let value = if self.consume_ident("case") {
            self.parse_set_case()?
        } else if self.consume_ident("if") {
            self.parse_set_if_else()?
        } else {
            SetValue::Expr(self.parse_expression()?)
        };
        Ok(PipelineNode::Set { field, value })
    }

    fn parse_set_case(&mut self) -> Result<SetValue, ParseError> {
        let mut when_clauses = Vec::new();
        while self.consume_ident("when") {
            let condition = self.parse_expression()?;
            self.expect_ident("then")?;
            let result = self.parse_value()?;
            when_clauses.push((condition, result));
        }
        let else_value = if self.consume_ident("else") {
            Some(self.parse_value()?)
        } else {
            None
        };
        self.expect_ident("end")?;
        Ok(SetValue::Case {
            when_clauses,
            else_value,
        })
    }

    fn parse_set_if_else(&mut self) -> Result<SetValue, ParseError> {
        let condition = self.parse_expression()?;
        self.expect_ident("then")?;
        let then_value = self.parse_value()?;
        let else_value = if self.consume_ident("else") {
            Some(self.parse_value()?)
        } else {
            None
        };
        Ok(SetValue::IfElse {
            condition,
            then_value,
            else_value,
        })
    }

    fn parse_field_list(&mut self) -> Result<Vec<String>, ParseError> {
        let mut fields = vec![self.expect_identifier_like("field")?];
        while self.consume(TokenKind::Comma) {
            fields.push(self.expect_identifier_like("field")?);
        }
        Ok(fields)
    }

    // ── Expression parsing with arithmetic precedence ──

    fn parse_expression(&mut self) -> Result<Expression, ParseError> {
        self.parse_or_expression()
    }

    fn parse_or_expression(&mut self) -> Result<Expression, ParseError> {
        let mut expr = self.parse_and_expression()?;
        while self.consume_ident("or") {
            let rhs = self.parse_and_expression()?;
            expr = Expression::Or(Box::new(expr), Box::new(rhs));
        }
        Ok(expr)
    }

    fn parse_and_expression(&mut self) -> Result<Expression, ParseError> {
        let mut expr = self.parse_not_expression()?;
        while self.consume_ident("and") {
            let rhs = self.parse_not_expression()?;
            expr = Expression::And(Box::new(expr), Box::new(rhs));
        }
        Ok(expr)
    }

    fn parse_not_expression(&mut self) -> Result<Expression, ParseError> {
        if self.consume_ident("not") {
            return Ok(Expression::Not(Box::new(self.parse_not_expression()?)));
        }
        self.parse_comparison_expression()
    }

    fn parse_comparison_expression(&mut self) -> Result<Expression, ParseError> {
        let left = self.parse_arith_expression()?;

        // Check for `in`
        if self.consume_ident("in") {
            self.expect(TokenKind::LParen, "`(`")?;
            let values = self.parse_value_list()?;
            self.expect(TokenKind::RParen, "`)`")?;
            return Ok(Expression::In {
                expr: Box::new(left),
                values,
            });
        }

        // Check for `is null` / `is not null`
        if self.consume_ident("is") {
            if self.consume_ident("not") {
                self.expect_ident("null")?;
                return Ok(Expression::IsNotNull(Box::new(left)));
            }
            self.expect_ident("null")?;
            return Ok(Expression::IsNull(Box::new(left)));
        }

        // Check for `between`
        if self.consume_ident("between") {
            let low = self.parse_arith_expression()?;
            self.expect_ident("and")?;
            let high = self.parse_arith_expression()?;
            return Ok(Expression::Between {
                expr: Box::new(left),
                low: Box::new(low),
                high: Box::new(high),
            });
        }

        // Check for comparison operator
        if let Some(op) = self.parse_optional_operator() {
            let right = self.parse_rhs_expression()?;
            return Ok(Expression::Comparison {
                left: Box::new(left),
                operator: op,
                right: Box::new(right),
            });
        }

        Ok(left)
    }

    /// Parse the right-hand side of a comparison, treating bare identifiers
    /// as literal values (not field references) for backward compatibility.
    fn parse_rhs_expression(&mut self) -> Result<Expression, ParseError> {
        // Try to parse as a simple value first
        if let Ok(value) = self.try_parse_rhs_value() {
            return Ok(Expression::Literal(value));
        }
        // Fall back to full expression (function calls, arithmetic, parens)
        self.parse_arith_expression()
    }

    /// Like try_parse_value but accepts ANY identifier (not just true/false)
    /// so that `state = running` treats `running` as a literal.
    fn try_parse_rhs_value(&mut self) -> Result<Value, ParseError> {
        let token = self
            .peek()
            .ok_or_else(|| self.error_here("expected value"))?
            .clone();
        match &token.kind {
            TokenKind::String(_) => {}
            TokenKind::Ident(_) => {}
            TokenKind::Integer(_) | TokenKind::Float(_) | TokenKind::Percentage(_) => {}
            _ => return Err(ParseError::new(token.column, "not a value")),
        }
        self.advance();
        match token.kind {
            TokenKind::String(value) => Ok(Value::String(value)),
            TokenKind::Ident(value) if value == "true" => Ok(Value::Boolean(true)),
            TokenKind::Ident(value) if value == "false" => Ok(Value::Boolean(false)),
            TokenKind::Ident(value) => Ok(Value::Identifier(value)),
            TokenKind::Integer(value) => Ok(Value::Integer(value)),
            TokenKind::Float(value) => Ok(Value::Float(value)),
            TokenKind::Percentage(value) => Ok(Value::Percentage(value)),
            _ => unreachable!(),
        }
    }

    fn parse_arith_expression(&mut self) -> Result<Expression, ParseError> {
        self.parse_term(0)
    }

    // Precedence climbing: + - (lowest), * / % (middle), unary - (highest)
    fn parse_term(&mut self, min_prec: u8) -> Result<Expression, ParseError> {
        let mut left = self.parse_unary_arith()?;

        loop {
            let op = match self.peek().map(|t| &t.kind) {
                Some(TokenKind::Plus) if 1 >= min_prec => {
                    self.advance();
                    BinOp::Add
                }
                Some(TokenKind::Minus) if 1 >= min_prec => {
                    self.advance();
                    BinOp::Sub
                }
                Some(TokenKind::Star) if 2 >= min_prec => {
                    self.advance();
                    BinOp::Mul
                }
                Some(TokenKind::Slash) if 2 >= min_prec => {
                    self.advance();
                    BinOp::Div
                }
                Some(TokenKind::Percent) if 2 >= min_prec => {
                    self.advance();
                    BinOp::Mod
                }
                _ => break,
            };
            let next_prec = match op {
                BinOp::Add | BinOp::Sub => 1,
                BinOp::Mul | BinOp::Div | BinOp::Mod => 2,
            };
            let right = self.parse_term(next_prec)?;
            left = Expression::Arithmetic {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }

        Ok(left)
    }

    fn parse_unary_arith(&mut self) -> Result<Expression, ParseError> {
        if self.consume(TokenKind::Minus) {
            let expr = self.parse_unary_arith()?;
            return Ok(Expression::Arithmetic {
                left: Box::new(Expression::Literal(Value::Integer(0))),
                op: BinOp::Sub,
                right: Box::new(expr),
            });
        }
        self.parse_primary_expression()
    }

    fn parse_primary_expression(&mut self) -> Result<Expression, ParseError> {
        // Parenthesized expression
        if self.consume(TokenKind::LParen) {
            let expr = self.parse_expression()?;
            self.expect(TokenKind::RParen, "`)`")?;
            return Ok(expr);
        }

        // Function call: ident "(" ...
        if let Some(Token {
            kind: TokenKind::Ident(name),
            ..
        }) = self.peek()
            && let Some(peek2) = self.tokens.get(self.cursor + 1)
            && matches!(peek2.kind, TokenKind::LParen)
        {
            let name = name.clone();
            self.advance(); // consume ident
            self.advance(); // consume LParen
            let mut args = Vec::new();
            if !self.check(TokenKind::RParen) {
                args.push(self.parse_expression()?);
                while self.consume(TokenKind::Comma) {
                    args.push(self.parse_expression()?);
                }
            }
            self.expect(TokenKind::RParen, "`)`")?;
            return Ok(Expression::FnCall { name, args });
        }

        // Try literal value first
        if let Ok(value) = self.try_parse_value() {
            return Ok(Expression::Literal(value));
        }

        // Field reference
        let field = self.expect_identifier_like("field")?;
        Ok(Expression::Field(field))
    }

    fn parse_value_list(&mut self) -> Result<Vec<Value>, ParseError> {
        let mut values = vec![self.parse_value()?];
        while self.consume(TokenKind::Comma) {
            values.push(self.parse_value()?);
        }
        Ok(values)
    }

    fn parse_fill_pipeline(&mut self) -> Result<PipelineNode, ParseError> {
        let field = self.expect_identifier_like("field name")?;
        self.expect_ident("with")?;
        let default = self.parse_expression()?;
        Ok(PipelineNode::Fill { field, default })
    }

    fn parse_optional_operator(&mut self) -> Option<Operator> {
        let op = match self.peek().map(|t| &t.kind) {
            Some(TokenKind::Eq) => Operator::Eq,
            Some(TokenKind::NotEq) => Operator::NotEq,
            Some(TokenKind::Gt) => Operator::Gt,
            Some(TokenKind::Lt) => Operator::Lt,
            Some(TokenKind::Gte) => Operator::Gte,
            Some(TokenKind::Lte) => Operator::Lte,
            Some(TokenKind::Ident(v)) if v == "contains" => Operator::Contains,
            Some(TokenKind::Ident(v)) if v == "matches" => Operator::Matches,
            Some(TokenKind::Ident(v)) if v == "starts_with" => Operator::StartsWith,
            Some(TokenKind::Ident(v)) if v == "ends_with" => Operator::EndsWith,
            _ => return None,
        };
        self.advance();
        Some(op)
    }

    fn try_parse_value(&mut self) -> Result<Value, ParseError> {
        let token = self
            .peek()
            .ok_or_else(|| self.error_here("expected value"))?
            .clone();
        match &token.kind {
            TokenKind::String(_) => {}
            TokenKind::Ident(v) if v == "true" || v == "false" => {}
            TokenKind::Integer(_) | TokenKind::Float(_) | TokenKind::Percentage(_) => {}
            _ => return Err(ParseError::new(token.column, "not a value")),
        }
        self.advance();
        match token.kind {
            TokenKind::String(value) => Ok(Value::String(value)),
            TokenKind::Ident(value) if value == "true" => Ok(Value::Boolean(true)),
            TokenKind::Ident(value) if value == "false" => Ok(Value::Boolean(false)),
            TokenKind::Ident(value) => Ok(Value::Identifier(value)),
            TokenKind::Integer(value) => Ok(Value::Integer(value)),
            TokenKind::Float(value) => Ok(Value::Float(value)),
            TokenKind::Percentage(value) => Ok(Value::Percentage(value)),
            _ => unreachable!(),
        }
    }

    fn parse_value(&mut self) -> Result<Value, ParseError> {
        let token = self
            .advance()
            .ok_or_else(|| self.error_here("expected value"))?;
        match token.kind {
            TokenKind::String(value) => Ok(Value::String(value)),
            TokenKind::Ident(value) if value == "true" => Ok(Value::Boolean(true)),
            TokenKind::Ident(value) if value == "false" => Ok(Value::Boolean(false)),
            TokenKind::Ident(value) => Ok(Value::Identifier(value)),
            TokenKind::Integer(value) => Ok(Value::Integer(value)),
            TokenKind::Float(value) => Ok(Value::Float(value)),
            TokenKind::Percentage(value) => Ok(Value::Percentage(value)),
            _ => Err(ParseError::new(token.column, "expected value")),
        }
    }

    fn expect_duration(&mut self) -> Result<Duration, ParseError> {
        let token = self
            .advance()
            .ok_or_else(|| self.error_here("expected duration such as `5m`"))?;
        match token.kind {
            TokenKind::Duration(d) => Ok(d),
            _ => Err(ParseError::new(
                token.column,
                "expected duration such as `5m`",
            )),
        }
    }

    fn expect_u64(&mut self, context: &str) -> Result<u64, ParseError> {
        let token = self
            .advance()
            .ok_or_else(|| self.error_here(format!("expected integer for {context}")))?;
        match token.kind {
            TokenKind::Integer(n) if n >= 0 => Ok(n as u64),
            TokenKind::Integer(_) => Err(ParseError::new(
                token.column,
                format!("{context} must be non-negative"),
            )),
            _ => Err(ParseError::new(
                token.column,
                format!("expected integer for {context}"),
            )),
        }
    }

    fn expect_string(&mut self, context: &str) -> Result<String, ParseError> {
        let token = self
            .advance()
            .ok_or_else(|| self.error_here(format!("expected string for {context}")))?;
        match token.kind {
            TokenKind::String(s) => Ok(s),
            _ => Err(ParseError::new(
                token.column,
                format!("expected a quoted string for {context}"),
            )),
        }
    }

    fn expect_timestamp_string(&mut self) -> Result<String, ParseError> {
        let token = self
            .advance()
            .ok_or_else(|| self.error_here("expected timestamp string"))?;
        match token.kind {
            TokenKind::String(s) => Ok(s),
            _ => Err(ParseError::new(
                token.column,
                "expected a quoted timestamp string",
            ))?,
        }
    }

    fn expect_identifier_like(&mut self, context: &str) -> Result<String, ParseError> {
        let token = self
            .advance()
            .ok_or_else(|| self.error_here(format!("expected {context}")))?;
        match token.kind {
            TokenKind::Ident(s) => Ok(s),
            TokenKind::String(s) => Ok(s),
            _ => Err(ParseError::new(token.column, format!("expected {context}"))),
        }
    }

    fn expect_ident(&mut self, expected: &str) -> Result<(), ParseError> {
        let token = self
            .advance()
            .ok_or_else(|| self.error_here(format!("expected `{expected}`")))?;
        match &token.kind {
            TokenKind::Ident(value) if value == expected => Ok(()),
            _ => Err(ParseError::new(
                token.column,
                format!("expected `{expected}`"),
            )),
        }
    }

    fn expect(&mut self, kind: TokenKind, description: &str) -> Result<(), ParseError> {
        let token = self
            .advance()
            .ok_or_else(|| self.error_here(format!("expected {description}")))?;
        if token.kind == kind {
            Ok(())
        } else {
            Err(ParseError::new(
                token.column,
                format!("expected {description}"),
            ))
        }
    }

    fn expect_eof(&mut self) -> Result<(), ParseError> {
        if self.peek().is_some() {
            let token = self.peek().unwrap();
            return Err(ParseError::new(
                token.column,
                format!("unexpected token `{:?}`", token.kind),
            ));
        }
        Ok(())
    }

    fn consume_ident(&mut self, expected: &str) -> bool {
        match self.peek().map(|t| &t.kind) {
            Some(TokenKind::Ident(v)) if v == expected => {
                self.advance();
                true
            }
            _ => false,
        }
    }

    fn consume(&mut self, kind: TokenKind) -> bool {
        match self.peek().map(|t| &t.kind) {
            Some(k) if *k == kind => {
                self.advance();
                true
            }
            _ => false,
        }
    }

    fn check(&self, kind: TokenKind) -> bool {
        self.peek().map(|t| &t.kind) == Some(&kind)
    }

    fn is_at_time_or_pipe_or_eof(&self) -> bool {
        match self.peek().map(|t| &t.kind) {
            None => true,
            Some(TokenKind::Pipe) => true,
            Some(TokenKind::Ident(v)) if v == "last" || v == "from" => true,
            _ => false,
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.cursor)
    }

    fn peek_ident(&self) -> Option<&str> {
        match self.peek()?.kind {
            TokenKind::Ident(ref s) => Some(s.as_str()),
            _ => None,
        }
    }

    fn advance(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.cursor).cloned()?;
        self.cursor += 1;
        Some(token)
    }

    fn error_here(&self, message: impl Into<String>) -> ParseError {
        let col = self.peek().map(|t| t.column).unwrap_or(self.eof_column);
        ParseError::new(col, message)
    }
}

// ── Tokenizer ──

fn tokenize(source: &str) -> Result<Vec<Token>, ParseError> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let col = i + 1;

        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }

        // Comment
        if chars[i] == '#' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        // Single-char tokens
        match chars[i] {
            '|' => {
                tokens.push(Token {
                    kind: TokenKind::Pipe,
                    column: col,
                });
                i += 1;
                continue;
            }
            ',' => {
                tokens.push(Token {
                    kind: TokenKind::Comma,
                    column: col,
                });
                i += 1;
                continue;
            }
            '(' => {
                tokens.push(Token {
                    kind: TokenKind::LParen,
                    column: col,
                });
                i += 1;
                continue;
            }
            ')' => {
                tokens.push(Token {
                    kind: TokenKind::RParen,
                    column: col,
                });
                i += 1;
                continue;
            }
            '+' => {
                tokens.push(Token {
                    kind: TokenKind::Plus,
                    column: col,
                });
                i += 1;
                continue;
            }
            '*' => {
                tokens.push(Token {
                    kind: TokenKind::Star,
                    column: col,
                });
                i += 1;
                continue;
            }
            '/' => {
                tokens.push(Token {
                    kind: TokenKind::Slash,
                    column: col,
                });
                i += 1;
                continue;
            }
            '%' => {
                tokens.push(Token {
                    kind: TokenKind::Percent,
                    column: col,
                });
                i += 1;
                continue;
            }
            _ => {}
        }

        // String
        if chars[i] == '"' {
            let start = i;
            i += 1;
            let mut s = String::new();
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 1;
                    match chars[i] {
                        '"' => s.push('"'),
                        '\\' => s.push('\\'),
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        c => {
                            s.push('\\');
                            s.push(c);
                        }
                    }
                } else {
                    s.push(chars[i]);
                }
                i += 1;
            }
            if i >= chars.len() {
                return Err(ParseError::new(start + 1, "unterminated string"));
            }
            i += 1; // closing "
            tokens.push(Token {
                kind: TokenKind::String(s),
                column: col,
            });
            continue;
        }

        // Numbers, percentages, durations, identifiers
        if chars[i].is_ascii_digit()
            || chars[i] == '-' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit()
        {
            let start = i;
            if chars[i] == '-' {
                i += 1;
            }
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let mut is_float = false;
            if i < chars.len() && chars[i] == '.' {
                is_float = true;
                i += 1;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let num_str: &str = &source[start..i];

            if i < chars.len() && chars[i] == '%' {
                i += 1;
                let val: f64 = num_str
                    .parse()
                    .map_err(|_| ParseError::new(col, "invalid percentage"))?;
                tokens.push(Token {
                    kind: TokenKind::Percentage(val),
                    column: col,
                });
            } else if i < chars.len() && matches!(chars[i], 's' | 'm' | 'h' | 'd') {
                let unit_char = chars[i];
                i += 1;
                let val: u64 = num_str
                    .parse()
                    .map_err(|_| ParseError::new(col, "invalid duration"))?;
                let unit = match unit_char {
                    's' => DurationUnit::Seconds,
                    'm' => DurationUnit::Minutes,
                    'h' => DurationUnit::Hours,
                    'd' => DurationUnit::Days,
                    _ => unreachable!(),
                };
                tokens.push(Token {
                    kind: TokenKind::Duration(Duration { value: val, unit }),
                    column: col,
                });
            } else if is_float {
                let val: f64 = num_str
                    .parse()
                    .map_err(|_| ParseError::new(col, "invalid float"))?;
                tokens.push(Token {
                    kind: TokenKind::Float(val),
                    column: col,
                });
            } else {
                let val: i64 = num_str
                    .parse()
                    .map_err(|_| ParseError::new(col, "invalid integer"))?;
                tokens.push(Token {
                    kind: TokenKind::Integer(val),
                    column: col,
                });
            }
            continue;
        }

        // Multi-char operators: >= <= !=
        if chars[i] == '>' && i + 1 < chars.len() && chars[i + 1] == '=' {
            tokens.push(Token {
                kind: TokenKind::Gte,
                column: col,
            });
            i += 2;
            continue;
        }
        if chars[i] == '<' && i + 1 < chars.len() && chars[i + 1] == '=' {
            tokens.push(Token {
                kind: TokenKind::Lte,
                column: col,
            });
            i += 2;
            continue;
        }
        if chars[i] == '!' && i + 1 < chars.len() && chars[i + 1] == '=' {
            tokens.push(Token {
                kind: TokenKind::NotEq,
                column: col,
            });
            i += 2;
            continue;
        }
        if chars[i] == '=' {
            tokens.push(Token {
                kind: TokenKind::Eq,
                column: col,
            });
            i += 1;
            continue;
        }
        if chars[i] == '>' {
            tokens.push(Token {
                kind: TokenKind::Gt,
                column: col,
            });
            i += 1;
            continue;
        }
        if chars[i] == '<' {
            tokens.push(Token {
                kind: TokenKind::Lt,
                column: col,
            });
            i += 1;
            continue;
        }
        if chars[i] == '-' && i + 1 < chars.len() && chars[i + 1] == '>' {
            tokens.push(Token {
                kind: TokenKind::Pipe,
                column: col,
            });
            i += 2;
            continue;
        }

        // Identifier
        if chars[i].is_ascii_alphanumeric()
            || chars[i] == '_'
            || chars[i] == '.'
            || chars[i] == ':'
            || chars[i] == '@'
            || chars[i] == '/'
            || chars[i] == '-'
        {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric()
                    || chars[i] == '_'
                    || chars[i] == '.'
                    || chars[i] == ':'
                    || chars[i] == '@'
                    || chars[i] == '/'
                    || chars[i] == '-')
            {
                i += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Ident(source[start..i].to_owned()),
                column: col,
            });
            continue;
        }

        return Err(ParseError::new(
            col,
            format!("unexpected character `{}`", chars[i]),
        ));
    }

    Ok(tokens)
}

// ── Parse tests remain the same ──
// Note: the inline where + pipeline test cases all produce the same AST shapes
// because the parser still emits the same high-level Query/Expression types.
// The internal structure changes (Field vs field string) but the tests
// only check high-level shapes (matches! patterns) which are preserved.

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_one(query: &str) -> ParsedQuery {
        parse(query).unwrap()
    }

    #[test]
    fn parses_observe_containers() {
        let q = parse_one("observe containers");
        assert!(matches!(q.query, Query::Observe(_)));
    }

    #[test]
    fn parses_inline_where() {
        let q = parse_one("observe containers where status = running");
        match q.query {
            Query::Observe(ref o) => assert!(o.filter.is_some()),
            _ => panic!("expected Observe"),
        }
    }

    #[test]
    fn parses_pipe_query() {
        let q = parse_one(
            "observe containers | where cpu > 80% | select name, image, cpu | sort cpu desc | limit 10",
        );
        assert!(matches!(q.query, Query::Observe(_)));
    }

    #[test]
    fn parses_fields_containers() {
        let q = parse_one("fields containers");
        assert!(matches!(
            q.query,
            Query::Fields(CollectionTarget::Containers)
        ));
    }

    #[test]
    fn parses_fields_images() {
        let q = parse_one("fields images");
        assert!(matches!(q.query, Query::Fields(CollectionTarget::Images)));
    }

    #[test]
    fn parses_events_query() {
        let q = parse_one("events containers");
        assert!(matches!(q.query, Query::Events(_)));
    }

    #[test]
    fn parses_inspect_at_query() {
        let q = parse_one(r#"inspect container api-service at "2026-01-01 12:00:00""#);
        assert!(matches!(q.query, Query::Inspect(_)));
    }

    #[test]
    fn parses_alert_query() {
        let q = parse_one(r#"alert when cpu > 85% for 2m then print "High CPU""#);
        assert!(matches!(q.query, Query::Alert(_)));
    }

    #[test]
    fn parses_analyze_query() {
        let q = parse_one("analyze containers find anomalies");
        assert!(matches!(q.query, Query::Analyze(_)));
    }

    #[test]
    fn parses_sort_and_limit() {
        let q = parse_one("observe containers | sort by state desc | limit 5");
        if let Query::Observe(ref o) = q.query {
            assert!(matches!(o.pipeline[0], PipelineNode::SortBy { .. }));
            assert!(matches!(o.pipeline[1], PipelineNode::Limit(5)));
        } else {
            panic!("expected Observe");
        }
    }

    #[test]
    fn parses_boolean_precedence() {
        let q =
            parse_one("observe containers where status = running and (cpu > 80% or memory > 90%)");
        let Query::Observe(ref o) = q.query else {
            panic!("expected Observe")
        };
        let filter = o.filter.as_ref().expect("expected filter");
        assert!(matches!(filter, Expression::And(_, _)));
    }

    #[test]
    fn parses_inline_where_expression() {
        let q = parse_one("observe containers where status = running and cpu > 80%");
        let Query::Observe(ref o) = q.query else {
            panic!("expected Observe")
        };
        assert!(matches!(o.filter, Some(Expression::And(_, _))));
    }

    #[test]
    fn parses_if_then_pipeline() {
        let q = parse_one(r#"observe containers | if cpu > 90% then alert "Critical CPU""#);
        let Query::Observe(ref o) = q.query else {
            panic!("expected Observe")
        };
        assert!(matches!(o.pipeline[0], PipelineNode::If { .. }));
    }

    #[test]
    fn parses_if_then_else_if() {
        let q = parse_one(
            r#"observe containers | if cpu > 90% then alert "Critical" else if cpu > 70% then alert "Warning""#,
        );
        let Query::Observe(ref o) = q.query else {
            panic!("expected Observe")
        };
        assert!(matches!(o.pipeline[0], PipelineNode::If { .. }));
    }

    #[test]
    fn parses_set_literal() {
        let q = parse_one(r#"observe containers | set tier = "prod""#);
        let Query::Observe(ref o) = q.query else {
            panic!("expected Observe")
        };
        assert!(matches!(o.pipeline[0], PipelineNode::Set { .. }));
    }

    #[test]
    fn parses_set_case_when() {
        let q = parse_one(
            r#"observe containers | set severity = case when cpu > 80% then "critical" else "ok" end"#,
        );
        let Query::Observe(ref o) = q.query else {
            panic!("expected Observe")
        };
        match &o.pipeline[0] {
            PipelineNode::Set { value, .. } => assert!(matches!(value, SetValue::Case { .. })),
            _ => panic!("expected Set with Case"),
        }
    }

    #[test]
    fn parses_set_if_else() {
        let q = parse_one(
            r#"observe containers | set health = if state = running then "up" else "down""#,
        );
        let Query::Observe(ref o) = q.query else {
            panic!("expected Observe")
        };
        match &o.pipeline[0] {
            PipelineNode::Set { value, .. } => assert!(matches!(value, SetValue::IfElse { .. })),
            _ => panic!("expected Set with IfElse"),
        }
    }

    #[test]
    fn parses_in_operator() {
        let q = parse_one(r#"observe containers | where image in ("nginx", "redis")"#);
        let Query::Observe(ref o) = q.query else {
            panic!("expected Observe")
        };
        assert!(matches!(
            o.pipeline[0],
            PipelineNode::Where(Expression::In { .. })
        ));
    }

    #[test]
    fn rejects_empty_query() {
        assert!(parse("").is_err());
        assert!(parse("   ").is_err());
    }

    #[test]
    fn reports_position_for_bad_where() {
        let err = parse("observe containers where").unwrap_err();
        assert!(err.column >= 22);
    }

    #[test]
    fn shows_context_with_pointer() {
        // Query with a trailing `where` (expects expression, finds EOF)
        let err = parse("observe containers where").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("-->"));
        assert!(
            msg.contains("^"),
            "error display should have a pointer: {msg}"
        );
        assert!(
            msg.contains("observe containers where"),
            "error display should show the source query: {msg}"
        );
    }

    #[test]
    fn shows_context_with_pointer_at_pipe() {
        // Query with `where` followed by pipe (expects expression, finds pipe)
        let err = parse("observe containers | where | sort by cpu").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("-->"));
        assert!(
            msg.contains("^"),
            "error display should have a pointer: {msg}"
        );
        assert!(
            msg.contains("observe containers | where | sort by cpu"),
            "error display should show the source query: {msg}"
        );
        // The pointer should be at or after the `where` keyword
        let pointer_pos = msg.find('^').unwrap();
        let source_pos = msg.find("where |").unwrap_or(msg.len());
        assert!(
            pointer_pos > source_pos,
            "pointer should be after `where |`: {msg}"
        );
    }

    #[test]
    fn all_example_files_parse() {
        use std::path::Path;
        let examples_dir = Path::new("examples");
        if !examples_dir.exists() {
            return;
        }
        let mut count = 0;
        for entry in std::fs::read_dir(examples_dir).unwrap() {
            let entry = entry.unwrap();
            if entry
                .path()
                .extension()
                .map(|e| e == "dol")
                .unwrap_or(false)
            {
                let content = std::fs::read_to_string(entry.path()).unwrap();
                let result = parse(&content);
                if let Err(ref e) = result {
                    panic!("Failed to parse {}: {}", entry.path().display(), e);
                }
                count += 1;
            }
        }
        assert!(count > 0, "no example files found");
    }

    // ── New Tier 1 parse tests ──

    #[test]
    fn parses_arithmetic_expression() {
        let q = parse_one(r#"observe containers | set mem_gb = memory / 1073741824"#);
        let Query::Observe(ref o) = q.query else {
            panic!("expected Observe")
        };
        match &o.pipeline[0] {
            PipelineNode::Set { value, .. } => {
                assert!(matches!(
                    value,
                    SetValue::Expr(Expression::Arithmetic { op: BinOp::Div, .. })
                ));
            }
            _ => panic!("expected Set with Expr"),
        }
    }

    #[test]
    fn parses_between() {
        let q = parse_one("observe containers where cpu between 50 and 80");
        let Query::Observe(ref o) = q.query else {
            panic!("expected Observe")
        };
        let filter = o.filter.as_ref().expect("expected filter");
        assert!(matches!(filter, Expression::Between { .. }));
    }

    #[test]
    fn parses_is_null() {
        let q = parse_one("observe containers where finished_at is null");
        let Query::Observe(ref o) = q.query else {
            panic!("expected Observe")
        };
        let filter = o.filter.as_ref().expect("expected filter");
        assert!(matches!(filter, Expression::IsNull(_)));
    }

    #[test]
    fn parses_is_not_null() {
        let q = parse_one("observe containers where finished_at is not null");
        let Query::Observe(ref o) = q.query else {
            panic!("expected Observe")
        };
        let filter = o.filter.as_ref().expect("expected filter");
        assert!(matches!(filter, Expression::IsNotNull(_)));
    }

    #[test]
    fn parses_function_call() {
        let q = parse_one("observe containers | where upper(name) contains \"API\"");
        let Query::Observe(ref o) = q.query else {
            panic!("expected Observe")
        };
        match &o.pipeline[0] {
            PipelineNode::Where(Expression::Comparison { left, .. }) => {
                assert!(
                    matches!(left.as_ref(), Expression::FnCall { name, .. } if name == "upper")
                );
            }
            _ => panic!("expected FnCall comparison"),
        }
    }

    #[test]
    fn parses_multi_field_sort() {
        let q = parse_one("observe containers | sort by state desc, cpu desc");
        let Query::Observe(ref o) = q.query else {
            panic!("expected Observe")
        };
        match &o.pipeline[0] {
            PipelineNode::SortBy { fields } => {
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0].0, "state");
                assert_eq!(fields[0].1, SortDirection::Desc);
                assert_eq!(fields[1].0, "cpu");
            }
            _ => panic!("expected SortBy"),
        }
    }

    #[test]
    fn parses_distinct() {
        let q = parse_one("observe containers | distinct | select image");
        let Query::Observe(ref o) = q.query else {
            panic!("expected Observe")
        };
        assert!(matches!(o.pipeline[0], PipelineNode::Distinct));
    }

    #[test]
    fn parses_compose_query() {
        let q = parse_one("compose myapp");
        assert!(matches!(q.query, Query::Compose(_)));
        if let Query::Compose(ref c) = q.query {
            assert_eq!(c.project, "myapp");
            assert_eq!(c.target, ComposeTarget::Containers);
            assert!(c.pipeline.is_empty());
        }
    }

    #[test]
    fn parses_compose_services() {
        let q = parse_one("compose myapp services");
        assert!(matches!(q.query, Query::Compose(_)));
        if let Query::Compose(ref c) = q.query {
            assert_eq!(c.project, "myapp");
            assert_eq!(c.target, ComposeTarget::Services);
        }
    }

    #[test]
    fn parses_observe_compose() {
        let q = parse_one("observe compose myapp");
        assert!(matches!(q.query, Query::Compose(_)));
        if let Query::Compose(ref c) = q.query {
            assert_eq!(c.project, "myapp");
            assert_eq!(c.target, ComposeTarget::Containers);
        }
    }

    #[test]
    fn parses_compose_with_pipeline() {
        let q = parse_one("compose myapp | where cpu > 80% | select name, cpu");
        assert!(matches!(q.query, Query::Compose(_)));
        if let Query::Compose(ref c) = q.query {
            assert_eq!(c.project, "myapp");
            assert_eq!(c.pipeline.len(), 2);
        }
    }

    #[test]
    fn parses_compose_containers_explicit() {
        let q = parse_one("compose myapp containers");
        if let Query::Compose(ref c) = q.query {
            assert_eq!(c.project, "myapp");
            assert_eq!(c.target, ComposeTarget::Containers);
        } else {
            panic!("expected Compose");
        }
    }

    #[test]
    fn parses_observe_compose_services() {
        let q = parse_one("observe compose myapp services");
        if let Query::Compose(ref c) = q.query {
            assert_eq!(c.project, "myapp");
            assert_eq!(c.target, ComposeTarget::Services);
        } else {
            panic!("expected Compose");
        }
    }

    #[test]
    fn parses_compose_with_sort_limit() {
        let q = parse_one("compose myapp services | sort by name asc | limit 5");
        if let Query::Compose(ref c) = q.query {
            assert_eq!(c.project, "myapp");
            assert_eq!(c.target, ComposeTarget::Services);
            assert_eq!(c.pipeline.len(), 2);
        } else {
            panic!("expected Compose");
        }
    }

    #[test]
    fn parses_compose_with_group_by() {
        let q = parse_one("compose myapp | group by state with count(id) as cnt");
        if let Query::Compose(ref c) = q.query {
            assert_eq!(c.pipeline.len(), 1);
            assert!(matches!(c.pipeline[0], PipelineNode::GroupBy { .. }));
        } else {
            panic!("expected Compose");
        }
    }

    #[test]
    fn parses_compose_with_where_and_select() {
        let q = parse_one("compose myapp | where state = \"running\" | select name, image, state");
        if let Query::Compose(ref c) = q.query {
            assert_eq!(c.pipeline.len(), 2);
            assert!(matches!(c.pipeline[0], PipelineNode::Where(_)));
            assert!(matches!(c.pipeline[1], PipelineNode::Select(_)));
        } else {
            panic!("expected Compose");
        }
    }

    #[test]
    fn parses_compose_with_distinct() {
        let q = parse_one("compose myapp | select image | distinct");
        if let Query::Compose(ref c) = q.query {
            assert_eq!(c.pipeline.len(), 2);
            assert!(matches!(c.pipeline[1], PipelineNode::Distinct));
        } else {
            panic!("expected Compose");
        }
    }

    #[test]
    fn parses_compose_hyphenated_project() {
        let q = parse_one("compose my-app-v2");
        if let Query::Compose(ref c) = q.query {
            assert_eq!(c.project, "my-app-v2");
        } else {
            panic!("expected Compose");
        }
    }

    #[test]
    fn rejects_compose_without_project() {
        let result = parse("compose");
        assert!(result.is_err());
    }

    #[test]
    fn parses_compose_networks() {
        let q = parse_one("compose myapp networks");
        if let Query::Compose(ref c) = q.query {
            assert_eq!(c.project, "myapp");
            assert_eq!(c.target, ComposeTarget::Networks);
        } else {
            panic!("expected Compose");
        }
    }

    #[test]
    fn parses_compose_volumes() {
        let q = parse_one("compose myapp volumes");
        if let Query::Compose(ref c) = q.query {
            assert_eq!(c.project, "myapp");
            assert_eq!(c.target, ComposeTarget::Volumes);
        } else {
            panic!("expected Compose");
        }
    }

    #[test]
    fn parses_compose_health() {
        let q = parse_one("compose myapp health");
        if let Query::Compose(ref c) = q.query {
            assert_eq!(c.project, "myapp");
            assert_eq!(c.target, ComposeTarget::Health);
        } else {
            panic!("expected Compose");
        }
    }

    #[test]
    fn parses_observe_compose_networks() {
        let q = parse_one("observe compose myapp networks | select name, driver");
        if let Query::Compose(ref c) = q.query {
            assert_eq!(c.project, "myapp");
            assert_eq!(c.target, ComposeTarget::Networks);
            assert_eq!(c.pipeline.len(), 1);
        } else {
            panic!("expected Compose");
        }
    }

    #[test]
    fn parses_observe_compose_health() {
        let q = parse_one("observe compose myapp health");
        if let Query::Compose(ref c) = q.query {
            assert_eq!(c.project, "myapp");
            assert_eq!(c.target, ComposeTarget::Health);
        } else {
            panic!("expected Compose");
        }
    }

    #[test]
    fn parses_offset() {
        let q = parse_one("observe containers | sort by name asc | offset 10 | limit 5");
        let Query::Observe(ref o) = q.query else {
            panic!("expected Observe")
        };
        assert!(matches!(o.pipeline[1], PipelineNode::Offset(10)));
    }

    #[test]
    fn parses_having() {
        let q =
            parse_one("observe containers | group by image with count(id) as cnt | having cnt > 3");
        let Query::Observe(ref o) = q.query else {
            panic!("expected Observe")
        };
        assert!(matches!(o.pipeline[1], PipelineNode::Having(_)));
    }

    #[test]
    fn parses_observe_join_networks() {
        let q = parse_one("observe containers join networks on id = containers");
        let Query::Observe(ref o) = q.query else {
            panic!("expected Observe")
        };
        let join = o.join.as_ref().expect("expected join clause");
        assert_eq!(join.right, CollectionTarget::Networks);
    }

    #[test]
    fn parses_observe_join_images() {
        let q = parse_one("observe containers join images on image = name");
        let Query::Observe(ref o) = q.query else {
            panic!("expected Observe")
        };
        assert!(o.join.is_some());
        assert_eq!(o.join.as_ref().unwrap().right, CollectionTarget::Images);
    }

    #[test]
    fn parses_observe_join_with_pipeline() {
        let q = parse_one(
            "observe containers join networks on id = containers | select c.name, n.name",
        );
        let Query::Observe(ref o) = q.query else {
            panic!("expected Observe")
        };
        assert!(o.join.is_some());
        assert_eq!(o.pipeline.len(), 1);
        assert!(matches!(o.pipeline[0], PipelineNode::Select(_)));
    }

    #[test]
    fn rejects_join_without_on() {
        let result = parse("observe containers join networks");
        assert!(result.is_err());
    }

    #[test]
    fn parses_fill_pipeline() {
        let q = parse_one("observe containers | fill memory with 0");
        let Query::Observe(ref o) = q.query else { panic!("expected Observe") };
        assert!(matches!(o.pipeline[0], PipelineNode::Fill { .. }));
    }

    #[test]
    fn parses_fill_with_expression() {
        let q = parse_one("observe containers | fill name with coalesce(label.name, name)");
        let Query::Observe(ref o) = q.query else { panic!("expected Observe") };
        assert!(matches!(o.pipeline[0], PipelineNode::Fill { .. }));
    }

    #[test]
    fn parses_starts_with_operator() {
        let q = parse_one("observe containers where name starts_with \"api-\"");
        let Query::Observe(ref o) = q.query else { panic!("expected Observe") };
        let filter = o.filter.as_ref().expect("expected filter");
        assert!(matches!(
            filter,
            Expression::Comparison { operator: Operator::StartsWith, .. }
        ));
    }

    #[test]
    fn parses_ends_with_operator() {
        let q = parse_one("observe containers where image ends_with \":latest\"");
        let Query::Observe(ref o) = q.query else { panic!("expected Observe") };
        let filter = o.filter.as_ref().expect("expected filter");
        assert!(matches!(
            filter,
            Expression::Comparison { operator: Operator::EndsWith, .. }
        ));
    }
}
