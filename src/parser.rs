use serde::Serialize;
use thiserror::Error;

use crate::ast::{
    AlertAction, AlertRule, AnalysisTarget, AnalysisVerb, CollectionTarget, Duration, DurationUnit,
    EventsQuery, Expression, InspectQuery, Operator, PipelineNode, Query, SetValue,
    SingularTarget, SingularTargetKind, SortDirection, TimeSelector, Value,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ParsedQuery {
    pub source: String,
    pub query: Query,
}

#[derive(Debug, Clone, Eq, PartialEq, Error)]
#[error("parse error at column {column}: {message}")]
pub struct ParseError {
    pub column: usize,
    pub message: String,
}

pub fn parse(source: &str) -> Result<ParsedQuery, ParseError> {
    let trimmed = source.trim();

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

    fn parse_query(&mut self) -> Result<Query, ParseError> {
        match self.peek_ident() {
            Some("observe") => self.parse_observe(),
            Some("events") => self.parse_events(),
            Some("inspect") => self.parse_inspect(),
            Some("analyze") => self.parse_analyze(),
            Some("alert") => self.parse_alert_rule().map(Query::Alert),
            Some(other) => Err(self.error_here(format!(
                "expected query family, found `{other}`; try `observe containers`"
            ))),
            None => Err(self.error_here("expected query family")),
        }
    }

    fn parse_observe(&mut self) -> Result<Query, ParseError> {
        self.expect_ident("observe")?;
        let target = self.parse_collection_target()?;
        let time = self.parse_optional_time_selector()?;
        let filter = self.parse_optional_inline_where()?;
        let pipeline = self.parse_pipeline()?;

        Ok(Query::Observe(crate::ast::ObserveQuery {
            target,
            time,
            filter,
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

    fn parse_alert_action(&mut self) -> Result<AlertAction, ParseError> {
        if self.consume_ident("print") {
            return Ok(AlertAction::Print(self.expect_string("print message")?));
        }

        if self.consume_ident("webhook") {
            return Ok(AlertAction::Webhook(self.expect_string("webhook URL")?));
        }

        if self.consume_ident("restart") {
            return Ok(AlertAction::Restart(self.parse_singular_target()?));
        }

        Err(self.error_here("expected alert action: `print`, `webhook`, or `restart`"))
    }

    fn parse_analysis_target(&mut self) -> Result<AnalysisTarget, ParseError> {
        if let Some(target) = self.try_parse_collection_target() {
            return Ok(AnalysisTarget::Collection(target));
        }

        self.parse_singular_target().map(AnalysisTarget::Singular)
    }

    fn parse_collection_target(&mut self) -> Result<CollectionTarget, ParseError> {
        self.try_parse_collection_target().ok_or_else(|| {
            self.error_here("expected collection target: containers, images, networks, or volumes")
        })
    }

    fn try_parse_collection_target(&mut self) -> Option<CollectionTarget> {
        let target = match self.peek_ident()? {
            "containers" => CollectionTarget::Containers,
            "images" => CollectionTarget::Images,
            "networks" => CollectionTarget::Networks,
            "volumes" => CollectionTarget::Volumes,
            _ => return None,
        };
        self.advance();
        Some(target)
    }

    fn parse_singular_target(&mut self) -> Result<SingularTarget, ParseError> {
        let kind = match self.peek_ident() {
            Some("container") => SingularTargetKind::Container,
            Some("image") => SingularTargetKind::Image,
            Some("network") => SingularTargetKind::Network,
            Some("volume") => SingularTargetKind::Volume,
            _ => {
                return Err(self
                    .error_here("expected singular target: container, image, network, or volume"));
            }
        };
        self.advance();
        let value = self.expect_value_text("target value")?;

        Ok(SingularTarget { kind, value })
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
            _ => Err(self.error_here("expected analysis verb: find, correlate, or explain")),
        }
    }

    fn parse_optional_time_selector(&mut self) -> Result<Option<TimeSelector>, ParseError> {
        if self.consume_ident("last") {
            return Ok(Some(TimeSelector::Last(self.expect_duration()?)));
        }

        if self.consume_ident("from") {
            let from = self.expect_timestamp_string()?;
            self.expect_ident("to")?;
            let to = self.expect_timestamp_string()?;
            return Ok(Some(TimeSelector::Range { from, to }));
        }

        Ok(None)
    }

    fn parse_optional_inline_where(&mut self) -> Result<Option<Expression>, ParseError> {
        if self.consume_ident("where") {
            return Ok(Some(self.parse_expression()?));
        }

        Ok(None)
    }

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
            return Ok(PipelineNode::GroupBy(self.parse_field_list()?));
        }

        if self.consume_ident("sort") {
            self.expect_ident("by")?;
            let field = self.expect_identifier_like("sort field")?;
            let direction = if self.consume_ident("desc") {
                SortDirection::Desc
            } else {
                self.consume_ident("asc");
                SortDirection::Asc
            };
            return Ok(PipelineNode::SortBy { field, direction });
        }

        if self.consume_ident("limit") {
            return Ok(PipelineNode::Limit(self.expect_u64("limit")?));
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

        Err(self.error_here(
            "expected pipeline node: where, select, group by, sort by, limit, alert, if, or set",
        ))
    }

    fn parse_if_pipeline(&mut self) -> Result<PipelineNode, ParseError> {
        let condition = self.parse_expression()?;
        self.expect_ident("then")?;
        let then_branch = self.parse_inline_pipeline_nodes()?;

        let else_branch = if self.consume_ident("else") {
            if self.consume_ident("if") {
                let nested = self.parse_if_pipeline()?;
                Some(vec![nested])
            } else if self.consume_ident("then") {
                Some(self.parse_inline_pipeline_nodes()?)
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
            SetValue::Literal(self.parse_value()?)
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

    fn parse_expression(&mut self) -> Result<Expression, ParseError> {
        self.parse_or_expression()
    }

    fn parse_or_expression(&mut self) -> Result<Expression, ParseError> {
        let mut expression = self.parse_and_expression()?;

        while self.consume_ident("or") {
            let rhs = self.parse_and_expression()?;
            expression = Expression::Or(Box::new(expression), Box::new(rhs));
        }

        Ok(expression)
    }

    fn parse_and_expression(&mut self) -> Result<Expression, ParseError> {
        let mut expression = self.parse_unary_expression()?;

        while self.consume_ident("and") {
            let rhs = self.parse_unary_expression()?;
            expression = Expression::And(Box::new(expression), Box::new(rhs));
        }

        Ok(expression)
    }

    fn parse_unary_expression(&mut self) -> Result<Expression, ParseError> {
        if self.consume_ident("not") {
            return Ok(Expression::Not(Box::new(self.parse_unary_expression()?)));
        }

        self.parse_primary_expression()
    }

    fn parse_primary_expression(&mut self) -> Result<Expression, ParseError> {
        if self.consume(TokenKind::LParen) {
            let expression = self.parse_expression()?;
            self.expect(TokenKind::RParen, "`)`")?;
            return Ok(expression);
        }

        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> Result<Expression, ParseError> {
        let field = self.expect_identifier_like("field")?;
        let operator = self.parse_operator()?;
        let value = self.parse_value()?;

        Ok(Expression::Comparison {
            field,
            operator,
            value,
        })
    }

    fn parse_operator(&mut self) -> Result<Operator, ParseError> {
        let operator = match self.peek().map(|token| &token.kind) {
            Some(TokenKind::Eq) => Operator::Eq,
            Some(TokenKind::NotEq) => Operator::NotEq,
            Some(TokenKind::Gt) => Operator::Gt,
            Some(TokenKind::Lt) => Operator::Lt,
            Some(TokenKind::Gte) => Operator::Gte,
            Some(TokenKind::Lte) => Operator::Lte,
            Some(TokenKind::Ident(value)) if value == "contains" => Operator::Contains,
            Some(TokenKind::Ident(value)) if value == "matches" => Operator::Matches,
            _ => {
                return Err(
                    self.error_here("expected operator: =, !=, >, <, >=, <=, contains, or matches")
                );
            }
        };
        self.advance();
        Ok(operator)
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
            TokenKind::Duration(duration) => Ok(duration),
            _ => Err(ParseError::new(
                token.column,
                "expected duration such as `5m`",
            )),
        }
    }

    fn expect_timestamp_string(&mut self) -> Result<String, ParseError> {
        self.expect_string("timestamp string")
    }

    fn expect_string(&mut self, label: &str) -> Result<String, ParseError> {
        let token = self
            .advance()
            .ok_or_else(|| self.error_here(format!("expected {label}")))?;

        match token.kind {
            TokenKind::String(value) => Ok(value),
            _ => Err(ParseError::new(token.column, format!("expected {label}"))),
        }
    }

    fn expect_identifier_like(&mut self, label: &str) -> Result<String, ParseError> {
        let token = self
            .advance()
            .ok_or_else(|| self.error_here(format!("expected {label}")))?;

        match token.kind {
            TokenKind::Ident(value) => Ok(value),
            _ => Err(ParseError::new(token.column, format!("expected {label}"))),
        }
    }

    fn expect_value_text(&mut self, label: &str) -> Result<String, ParseError> {
        let token = self
            .advance()
            .ok_or_else(|| self.error_here(format!("expected {label}")))?;

        match token.kind {
            TokenKind::Ident(value) | TokenKind::String(value) => Ok(value),
            _ => Err(ParseError::new(token.column, format!("expected {label}"))),
        }
    }

    fn expect_u64(&mut self, label: &str) -> Result<u64, ParseError> {
        let token = self
            .advance()
            .ok_or_else(|| self.error_here(format!("expected {label} integer")))?;

        match token.kind {
            TokenKind::Integer(value) if value >= 0 => Ok(value as u64),
            _ => Err(ParseError::new(
                token.column,
                format!("expected {label} integer"),
            )),
        }
    }

    fn expect_ident(&mut self, expected: &str) -> Result<(), ParseError> {
        if self.consume_ident(expected) {
            return Ok(());
        }

        Err(self.error_here(format!("expected `{expected}`")))
    }

    fn expect(&mut self, expected: TokenKind, label: &str) -> Result<(), ParseError> {
        if self.consume(expected) {
            return Ok(());
        }

        Err(self.error_here(format!("expected {label}")))
    }

    fn expect_eof(&self) -> Result<(), ParseError> {
        if let Some(token) = self.peek() {
            return Err(ParseError::new(
                token.column,
                "unexpected token; expected end of query",
            ));
        }

        Ok(())
    }

    fn is_at_time_or_pipe_or_eof(&self) -> bool {
        matches!(self.peek_ident(), Some("last" | "from"))
            || matches!(self.peek().map(|t| &t.kind), Some(TokenKind::Pipe) | None)
    }

    fn consume_ident(&mut self, expected: &str) -> bool {
        if self.peek_ident() == Some(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn consume(&mut self, expected: TokenKind) -> bool {
        if self.peek().map(|token| &token.kind) == Some(&expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn peek_ident(&self) -> Option<&str> {
        match self.peek().map(|token| &token.kind) {
            Some(TokenKind::Ident(value)) => Some(value.as_str()),
            _ => None,
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.cursor)
    }

    fn advance(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.cursor).cloned();
        if token.is_some() {
            self.cursor += 1;
        }
        token
    }

    fn error_here(&self, message: impl Into<String>) -> ParseError {
        let column = self.peek().map_or(self.eof_column, |token| token.column);
        ParseError::new(column, message)
    }
}

impl ParseError {
    fn new(column: usize, message: impl Into<String>) -> Self {
        Self {
            column,
            message: message.into(),
        }
    }
}

fn tokenize(source: &str) -> Result<Vec<Token>, ParseError> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < chars.len() {
        let c = chars[index];

        if c.is_whitespace() {
            index += 1;
            continue;
        }

        let column = index + 1;

        match c {
            '"' => {
                let (value, next_index) = read_string(&chars, index)?;
                tokens.push(Token {
                    kind: TokenKind::String(value),
                    column,
                });
                index = next_index;
            }
            '=' => {
                tokens.push(Token {
                    kind: TokenKind::Eq,
                    column,
                });
                index += 1;
            }
            '!' if chars.get(index + 1) == Some(&'=') => {
                tokens.push(Token {
                    kind: TokenKind::NotEq,
                    column,
                });
                index += 2;
            }
            '>' if chars.get(index + 1) == Some(&'=') => {
                tokens.push(Token {
                    kind: TokenKind::Gte,
                    column,
                });
                index += 2;
            }
            '<' if chars.get(index + 1) == Some(&'=') => {
                tokens.push(Token {
                    kind: TokenKind::Lte,
                    column,
                });
                index += 2;
            }
            '>' => {
                tokens.push(Token {
                    kind: TokenKind::Gt,
                    column,
                });
                index += 1;
            }
            '<' => {
                tokens.push(Token {
                    kind: TokenKind::Lt,
                    column,
                });
                index += 1;
            }
            '|' => {
                tokens.push(Token {
                    kind: TokenKind::Pipe,
                    column,
                });
                index += 1;
            }
            ',' => {
                tokens.push(Token {
                    kind: TokenKind::Comma,
                    column,
                });
                index += 1;
            }
            '(' => {
                tokens.push(Token {
                    kind: TokenKind::LParen,
                    column,
                });
                index += 1;
            }
            ')' => {
                tokens.push(Token {
                    kind: TokenKind::RParen,
                    column,
                });
                index += 1;
            }
            '0'..='9' => {
                let (kind, next_index) = read_numberish(&chars, index)?;
                tokens.push(Token { kind, column });
                index = next_index;
            }
            _ if is_ident_char(c) => {
                let (value, next_index) = read_identifier(&chars, index);
                tokens.push(Token {
                    kind: TokenKind::Ident(value),
                    column,
                });
                index = next_index;
            }
            _ => {
                return Err(ParseError::new(
                    column,
                    format!("unexpected character `{c}`"),
                ));
            }
        }
    }

    Ok(tokens)
}

fn read_string(chars: &[char], start: usize) -> Result<(String, usize), ParseError> {
    let mut index = start + 1;
    let mut value = String::new();

    while let Some(c) = chars.get(index).copied() {
        match c {
            '"' => return Ok((value, index + 1)),
            '\\' => {
                let escaped = chars.get(index + 1).copied().ok_or_else(|| {
                    ParseError::new(index + 1, "unterminated escape sequence in string")
                })?;
                value.push(match escaped {
                    '"' => '"',
                    '\\' => '\\',
                    'n' => '\n',
                    't' => '\t',
                    other => other,
                });
                index += 2;
            }
            other => {
                value.push(other);
                index += 1;
            }
        }
    }

    Err(ParseError::new(start + 1, "unterminated string"))
}

fn read_numberish(chars: &[char], start: usize) -> Result<(TokenKind, usize), ParseError> {
    let mut index = start;
    while matches!(chars.get(index), Some('0'..='9')) {
        index += 1;
    }

    let mut is_float = false;
    if chars.get(index) == Some(&'.') && matches!(chars.get(index + 1), Some('0'..='9')) {
        is_float = true;
        index += 1;
        while matches!(chars.get(index), Some('0'..='9')) {
            index += 1;
        }
    }

    let number_text: String = chars[start..index].iter().collect();

    if chars.get(index) == Some(&'%') {
        let value = number_text
            .parse::<f64>()
            .map_err(|_| ParseError::new(start + 1, "invalid percentage"))?;
        return Ok((TokenKind::Percentage(value), index + 1));
    }

    if let Some(unit) = chars.get(index).copied().and_then(DurationUnit::from_char) {
        if is_float {
            return Err(ParseError::new(
                start + 1,
                "duration value must be an integer",
            ));
        }
        let value = number_text
            .parse::<u64>()
            .map_err(|_| ParseError::new(start + 1, "invalid duration"))?;
        return Ok((TokenKind::Duration(Duration { value, unit }), index + 1));
    }

    if is_float {
        let value = number_text
            .parse::<f64>()
            .map_err(|_| ParseError::new(start + 1, "invalid float"))?;
        return Ok((TokenKind::Float(value), index));
    }

    let value = number_text
        .parse::<i64>()
        .map_err(|_| ParseError::new(start + 1, "invalid integer"))?;
    Ok((TokenKind::Integer(value), index))
}

fn read_identifier(chars: &[char], start: usize) -> (String, usize) {
    let mut index = start;

    while matches!(chars.get(index), Some(c) if is_ident_char(*c)) {
        index += 1;
    }

    (chars[start..index].iter().collect(), index)
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '/' | '@')
}

impl DurationUnit {
    fn from_char(value: char) -> Option<Self> {
        match value {
            's' => Some(Self::Seconds),
            'm' => Some(Self::Minutes),
            'h' => Some(Self::Hours),
            'd' => Some(Self::Days),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{ObserveQuery, SortDirection};

    #[test]
    fn parses_observe_containers() {
        let parsed = parse("observe containers").expect("query should parse");

        assert_eq!(
            parsed.query,
            Query::Observe(ObserveQuery {
                target: CollectionTarget::Containers,
                time: None,
                filter: None,
                pipeline: vec![],
            })
        );
    }

    #[test]
    fn parses_inline_where() {
        let parsed =
            parse("observe containers where status = running").expect("query should parse");

        let Query::Observe(query) = parsed.query else {
            panic!("expected observe query");
        };

        assert!(query.filter.is_some());
    }

    #[test]
    fn parses_pipe_query() {
        let parsed =
            parse("observe containers | where image contains \"postgres\" | select name, status")
                .expect("query should parse");

        let Query::Observe(query) = parsed.query else {
            panic!("expected observe query");
        };

        assert_eq!(query.pipeline.len(), 2);
        assert!(matches!(query.pipeline[0], PipelineNode::Where(_)));
        assert_eq!(
            query.pipeline[1],
            PipelineNode::Select(vec!["name".to_owned(), "status".to_owned()])
        );
    }

    #[test]
    fn parses_sort_and_limit() {
        let parsed =
            parse("observe images | sort by size desc | limit 10").expect("query should parse");

        let Query::Observe(query) = parsed.query else {
            panic!("expected observe query");
        };

        assert_eq!(
            query.pipeline,
            vec![
                PipelineNode::SortBy {
                    field: "size".to_owned(),
                    direction: SortDirection::Desc,
                },
                PipelineNode::Limit(10),
            ]
        );
    }

    #[test]
    fn parses_events_query() {
        let parsed = parse("events containers where action = \"die\"").expect("query should parse");

        assert!(matches!(parsed.query, Query::Events(_)));
    }

    #[test]
    fn parses_inspect_at_query() {
        let parsed =
            parse("inspect container api-service at \"2026-01-01 12:00:00\"").expect("query");

        let Query::Inspect(query) = parsed.query else {
            panic!("expected inspect query");
        };

        assert_eq!(query.target.value, "api-service");
        assert_eq!(query.at.as_deref(), Some("2026-01-01 12:00:00"));
    }

    #[test]
    fn parses_analyze_query() {
        let parsed = parse("analyze containers find restart_loops last 10m").expect("query");

        let Query::Analyze(query) = parsed.query else {
            panic!("expected analyze query");
        };

        assert_eq!(query.subject.as_deref(), Some("restart_loops"));
        assert!(matches!(query.time, Some(TimeSelector::Last(_))));
    }

    #[test]
    fn parses_alert_query() {
        let parsed = parse("alert when cpu > 85% for 2m then print \"High CPU\"").expect("query");

        assert!(matches!(parsed.query, Query::Alert(_)));
    }

    #[test]
    fn parses_boolean_precedence() {
        let parsed =
            parse("observe containers where status = running and (cpu > 80% or memory > 90%)")
                .expect("query should parse");

        let Query::Observe(query) = parsed.query else {
            panic!("expected observe query");
        };

        assert!(matches!(query.filter, Some(Expression::And(_, _))));
    }

    #[test]
    fn rejects_empty_query() {
        let error = parse(" ").unwrap_err();

        assert_eq!(error.column, 1);
        assert!(error.message.contains("empty DOL query"));
    }

    #[test]
    fn reports_position_for_bad_where() {
        let error = parse("observe containers where").unwrap_err();

        assert!(error.column >= 24);
        assert!(error.message.contains("expected field"));
    }

    #[test]
    fn all_example_files_parse() {
        let examples_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
        let entries = std::fs::read_dir(&examples_dir)
            .expect("examples directory should exist")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "dol"))
            .collect::<Vec<_>>();

        assert!(!entries.is_empty(), "examples directory should contain .dol files");

        for entry in &entries {
            let content = std::fs::read_to_string(entry.path())
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", entry.path().display()));
            let name = entry.path().file_name().unwrap().to_string_lossy().to_string();
            parse(&content).unwrap_or_else(|e| {
                panic!("{} should parse but got: {} at column {}", name, e.message, e.column)
            });
        }
    }

    #[test]
    fn parses_set_literal() {
        let Query::Observe(q) = parse("observe containers | set tier = \"prod\"").unwrap().query else {
            panic!("expected observe");
        };
        let PipelineNode::Set { field, value } = &q.pipeline[0] else {
            panic!("expected set node");
        };
        assert_eq!(field, "tier");
        assert!(matches!(value, SetValue::Literal(Value::String(s)) if s == "prod"));
    }

    #[test]
    fn parses_set_case_when() {
        let Query::Observe(q) = parse(
            "observe containers | set severity = case when cpu > 80% then \"critical\" when cpu > 50% then \"warning\" else \"ok\" end",
        ).unwrap().query else {
            panic!("expected observe");
        };
        let PipelineNode::Set { field, value } = &q.pipeline[0] else {
            panic!("expected set node");
        };
        assert_eq!(field, "severity");
        let SetValue::Case { when_clauses, else_value } = value else {
            panic!("expected case");
        };
        assert_eq!(when_clauses.len(), 2);
        assert!(matches!(else_value, Some(Value::String(s)) if s == "ok"));
    }

    #[test]
    fn parses_set_if_else() {
        let Query::Observe(q) = parse(
            "observe containers | set health = if state = running then \"up\" else \"down\"",
        ).unwrap().query else {
            panic!("expected observe");
        };
        let PipelineNode::Set { field, value } = &q.pipeline[0] else {
            panic!("expected set node");
        };
        assert_eq!(field, "health");
        assert!(matches!(value, SetValue::IfElse { .. }));
    }

    #[test]
    fn parses_if_then_pipeline() {
        let Query::Observe(q) = parse(
            "observe containers | if cpu > 80% then alert \"High CPU\" else select name, cpu",
        ).unwrap().query else {
            panic!("expected observe");
        };
        let PipelineNode::If { condition, then_branch, else_branch } = &q.pipeline[0] else {
            panic!("expected if node");
        };
        assert!(matches!(condition, Expression::Comparison { .. }));
        assert!(matches!(then_branch[0], PipelineNode::Alert(_)));
        assert!(else_branch.is_some());
    }

    #[test]
    fn parses_if_then_else_if() {
        let Query::Observe(q) = parse(
            "observe containers | if cpu > 90% then alert \"Critical\" else if cpu > 70% then alert \"Warning\"",
        ).unwrap().query else {
            panic!("expected observe");
        };
        let PipelineNode::If { else_branch, .. } = &q.pipeline[0] else {
            panic!("expected if node");
        };
        let nested = else_branch.as_ref().expect("should have else branch");
        assert!(matches!(nested[0], PipelineNode::If { .. }));
    }
}
