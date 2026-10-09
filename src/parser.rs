//! Parser descendente recursivo: tokens → `Statement`.
//! Identificadores são normalizados para minúsculas.

use crate::ast::*;
use crate::error::{Error, Result};
use crate::lexer::{tokenize, Token};
use crate::sched::Priority;
use crate::schema::Column;
use crate::value::{DataType, Value};

/// Interpreta um texto com um ou mais comandos separados por `;`.
///
/// Identificadores são normalizados para minúsculas; palavras-chave não diferenciam
/// maiúsculas de minúsculas.
///
/// # Errors
/// `Error::Parse` com a descrição do primeiro problema encontrado.
///
/// # Exemplo
/// ```
/// use rustysql::{ast::Statement, parser::parse};
///
/// let stmts = parse("BEGIN; SELECT 1 + 1 FROM t; COMMIT").unwrap();
/// assert_eq!(stmts.len(), 3);
/// assert!(matches!(stmts[0], Statement::Begin));
/// assert!(parse("SELEC * FROM t").is_err());
/// ```
pub fn parse(sql: &str) -> Result<Vec<Statement>> {
    let mut p = Parser { toks: tokenize(sql)?, pos: 0 };
    let mut stmts = Vec::new();
    loop {
        while p.eat(&Token::Semi) {}
        if p.peek().is_none() {
            break;
        }
        stmts.push(p.statement()?);
        if p.peek().is_some() && !p.eat(&Token::Semi) {
            return Err(Error::parse(format!("token inesperado {:?}", p.peek().unwrap())));
        }
    }
    Ok(stmts)
}

const RESERVED: &[&str] = &[
    "select", "from", "where", "group", "order", "by", "having", "limit", "offset", "join",
    "inner", "left", "outer", "on", "set", "values", "and", "or", "not", "as", "union",
];

struct Parser {
    toks: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos)
    }

    fn peek_at(&self, n: usize) -> Option<&Token> {
        self.toks.get(self.pos + n)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, t: &Token) -> bool {
        if self.peek() == Some(t) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, t: Token) -> Result<()> {
        if self.eat(&t) {
            Ok(())
        } else {
            Err(Error::parse(format!("esperava {:?}, encontrei {:?}", t, self.peek())))
        }
    }

    fn kw_at(&self, n: usize, kw: &str) -> bool {
        matches!(self.peek_at(n), Some(Token::Ident(s)) if s.eq_ignore_ascii_case(kw))
    }

    fn peek_kw(&self, kw: &str) -> bool {
        self.kw_at(0, kw)
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.peek_kw(kw) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect_kw(&mut self, kw: &str) -> Result<()> {
        if self.eat_kw(kw) {
            Ok(())
        } else {
            Err(Error::parse(format!("esperava {kw}, encontrei {:?}", self.peek())))
        }
    }

    fn ident(&mut self) -> Result<String> {
        match self.next() {
            Some(Token::Ident(s)) => Ok(s.to_ascii_lowercase()),
            other => Err(Error::parse(format!("esperava identificador, encontrei {other:?}"))),
        }
    }

    /// Identificador que pode ser um alias (não pode ser palavra reservada).
    fn peek_alias(&self) -> bool {
        matches!(self.peek(), Some(Token::Ident(s)) if !RESERVED.contains(&s.to_ascii_lowercase().as_str()))
    }

    // ---- comandos -------------------------------------------------------

    fn statement(&mut self) -> Result<Statement> {
        if self.eat_kw("EXPLAIN") {
            return Ok(Statement::Explain(Box::new(self.statement()?)));
        }
        if self.eat_kw("CREATE") {
            self.create()
        } else if self.eat_kw("DROP") {
            if self.eat_kw("TABLE") {
                Ok(Statement::DropTable { name: self.ident()? })
            } else {
                self.expect_kw("INDEX")?;
                Ok(Statement::DropIndex { name: self.ident()? })
            }
        } else if self.eat_kw("INSERT") {
            self.insert()
        } else if self.eat_kw("SELECT") {
            Ok(Statement::Select(Box::new(self.select()?)))
        } else if self.eat_kw("UPDATE") {
            self.update()
        } else if self.eat_kw("DELETE") {
            self.expect_kw("FROM")?;
            let table = self.ident()?;
            Ok(Statement::Delete { table, filter: self.where_clause()? })
        } else if self.eat_kw("BEGIN") {
            if !self.eat_kw("TRANSACTION") {
                self.eat_kw("WORK");
            }
            Ok(Statement::Begin)
        } else if self.eat_kw("COMMIT") {
            Ok(Statement::Commit)
        } else if self.eat_kw("ROLLBACK") {
            Ok(Statement::Rollback)
        } else if self.eat_kw("CHECKPOINT") {
            Ok(Statement::Checkpoint)
        } else if self.eat_kw("SET") {
            self.expect_kw("PRIORITY")?;
            let name = self.ident()?;
            Priority::parse(&name)
                .map(Statement::SetPriority)
                .ok_or_else(|| Error::parse(format!("prioridade desconhecida '{name}' (use LOW, NORMAL ou HIGH)")))
        } else {
            Err(Error::parse(format!("comando desconhecido: {:?}", self.peek())))
        }
    }

    fn create(&mut self) -> Result<Statement> {
        let unique = self.eat_kw("UNIQUE");
        if self.eat_kw("INDEX") {
            let name = self.ident()?;
            self.expect_kw("ON")?;
            let table = self.ident()?;
            self.expect(Token::LParen)?;
            let column = self.ident()?;
            self.expect(Token::RParen)?;
            return Ok(Statement::CreateIndex { name, table, column, unique });
        }
        if unique {
            return Err(Error::parse("UNIQUE só vale para CREATE INDEX"));
        }
        self.create_table()
    }

    fn create_table(&mut self) -> Result<Statement> {
        self.expect_kw("TABLE")?;
        let name = self.ident()?;
        self.expect(Token::LParen)?;
        let mut columns = Vec::new();
        loop {
            let col_name = self.ident()?;
            let ty_name = self.ident()?;
            let ty = DataType::parse(&ty_name)
                .ok_or_else(|| Error::parse(format!("tipo desconhecido '{ty_name}'")))?;
            if self.eat(&Token::LParen) {
                // VARCHAR(255): o tamanho é aceito e ignorado
                match self.next() {
                    Some(Token::Int(_)) => {}
                    other => return Err(Error::parse(format!("tamanho inválido: {other:?}"))),
                }
                self.expect(Token::RParen)?;
            }
            let mut col = Column { name: col_name, ty, not_null: false, primary_key: false };
            loop {
                if self.eat_kw("PRIMARY") {
                    self.expect_kw("KEY")?;
                    col.primary_key = true;
                    col.not_null = true;
                } else if self.eat_kw("NOT") {
                    self.expect_kw("NULL")?;
                    col.not_null = true;
                } else if self.eat_kw("NULL") {
                } else {
                    break;
                }
            }
            columns.push(col);
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        self.expect(Token::RParen)?;
        Ok(Statement::CreateTable { name, columns })
    }

    fn insert(&mut self) -> Result<Statement> {
        self.expect_kw("INTO")?;
        let table = self.ident()?;
        let mut columns = None;
        if self.eat(&Token::LParen) {
            let mut names = vec![self.ident()?];
            while self.eat(&Token::Comma) {
                names.push(self.ident()?);
            }
            self.expect(Token::RParen)?;
            columns = Some(names);
        }
        self.expect_kw("VALUES")?;
        let mut rows = Vec::new();
        loop {
            self.expect(Token::LParen)?;
            let mut row = vec![self.expr()?];
            while self.eat(&Token::Comma) {
                row.push(self.expr()?);
            }
            self.expect(Token::RParen)?;
            rows.push(row);
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        Ok(Statement::Insert { table, columns, rows })
    }

    fn table_ref(&mut self) -> Result<TableRef> {
        let name = self.ident()?;
        let alias = if self.eat_kw("AS") || self.peek_alias() { Some(self.ident()?) } else { None };
        Ok(TableRef { name, alias })
    }

    fn select(&mut self) -> Result<SelectStmt> {
        let mut items = Vec::new();
        loop {
            items.push(if self.eat(&Token::Star) {
                SelectItem::Star
            } else if matches!(self.peek(), Some(Token::Ident(_)))
                && self.peek_at(1) == Some(&Token::Dot)
                && self.peek_at(2) == Some(&Token::Star)
            {
                let t = self.ident()?;
                self.pos += 2;
                SelectItem::QualifiedStar(t)
            } else {
                let expr = self.expr()?;
                let alias = if self.eat_kw("AS") { Some(self.ident()?) } else { None };
                SelectItem::Expr { expr, alias }
            });
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        self.expect_kw("FROM")?;
        let from = self.table_ref()?;

        let mut joins = Vec::new();
        loop {
            let kind = if self.eat_kw("LEFT") {
                self.eat_kw("OUTER");
                JoinKind::Left
            } else if self.eat_kw("INNER") || self.peek_kw("JOIN") {
                JoinKind::Inner
            } else {
                break;
            };
            self.expect_kw("JOIN")?;
            let table = self.table_ref()?;
            self.expect_kw("ON")?;
            joins.push(Join { kind, table, on: self.expr()? });
        }

        let filter = self.where_clause()?;
        let mut group_by = Vec::new();
        if self.eat_kw("GROUP") {
            self.expect_kw("BY")?;
            group_by.push(self.expr()?);
            while self.eat(&Token::Comma) {
                group_by.push(self.expr()?);
            }
        }
        let having = if self.eat_kw("HAVING") { Some(self.expr()?) } else { None };

        let mut order_by = Vec::new();
        if self.eat_kw("ORDER") {
            self.expect_kw("BY")?;
            loop {
                let e = self.expr()?;
                let desc = if self.eat_kw("DESC") {
                    true
                } else {
                    self.eat_kw("ASC");
                    false
                };
                order_by.push((e, desc));
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
        }

        let (mut limit, mut offset) = (None, 0usize);
        let number = |p: &mut Parser, what: &str| match p.next() {
            Some(Token::Int(n)) if n >= 0 => Ok(n as usize),
            other => Err(Error::parse(format!("{what} inválido: {other:?}"))),
        };
        if self.eat_kw("LIMIT") {
            limit = Some(number(self, "LIMIT")?);
        }
        if self.eat_kw("OFFSET") {
            offset = number(self, "OFFSET")?;
        }
        Ok(SelectStmt { items, from, joins, filter, group_by, having, order_by, limit, offset })
    }

    fn update(&mut self) -> Result<Statement> {
        let table = self.ident()?;
        self.expect_kw("SET")?;
        let mut sets = Vec::new();
        loop {
            let col = self.ident()?;
            self.expect(Token::Eq)?;
            sets.push((col, self.expr()?));
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        Ok(Statement::Update { table, sets, filter: self.where_clause()? })
    }

    fn where_clause(&mut self) -> Result<Option<Expr>> {
        if self.eat_kw("WHERE") {
            Ok(Some(self.expr()?))
        } else {
            Ok(None)
        }
    }

    // ---- expressões (OR < AND < NOT < comparação < +- < */ < unário) ----

    fn expr(&mut self) -> Result<Expr> {
        let mut left = self.and_expr()?;
        while self.eat_kw("OR") {
            left = Expr::binary(BinOp::Or, left, self.and_expr()?);
        }
        Ok(left)
    }

    fn and_expr(&mut self) -> Result<Expr> {
        let mut left = self.not_expr()?;
        while self.eat_kw("AND") {
            left = Expr::binary(BinOp::And, left, self.not_expr()?);
        }
        Ok(left)
    }

    fn not_expr(&mut self) -> Result<Expr> {
        if self.eat_kw("NOT") {
            Ok(Expr::Unary { op: UnOp::Not, expr: Box::new(self.not_expr()?) })
        } else {
            self.comparison()
        }
    }

    fn comparison(&mut self) -> Result<Expr> {
        let left = self.additive()?;
        if self.eat_kw("IS") {
            let negated = self.eat_kw("NOT");
            self.expect_kw("NULL")?;
            return Ok(Expr::IsNull { expr: Box::new(left), negated });
        }

        let negated = self.peek_kw("NOT")
            && (self.kw_at(1, "BETWEEN") || self.kw_at(1, "IN") || self.kw_at(1, "LIKE"));
        if negated {
            self.pos += 1;
        }
        let wrap = |e: Expr| if negated { Expr::Unary { op: UnOp::Not, expr: Box::new(e) } } else { e };

        if self.eat_kw("BETWEEN") {
            let lo = self.additive()?;
            self.expect_kw("AND")?;
            let hi = self.additive()?;
            let both = Expr::binary(
                BinOp::And,
                Expr::binary(BinOp::Ge, left.clone(), lo),
                Expr::binary(BinOp::Le, left, hi),
            );
            return Ok(wrap(both));
        }
        if self.eat_kw("IN") {
            self.expect(Token::LParen)?;
            let mut any: Option<Expr> = None;
            loop {
                let eq = Expr::binary(BinOp::Eq, left.clone(), self.additive()?);
                any = Some(match any {
                    None => eq,
                    Some(a) => Expr::binary(BinOp::Or, a, eq),
                });
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
            self.expect(Token::RParen)?;
            return Ok(wrap(any.unwrap()));
        }
        if self.eat_kw("LIKE") {
            let pattern = self.additive()?;
            return Ok(wrap(Expr::Like { expr: Box::new(left), pattern: Box::new(pattern), negated: false }));
        }

        let op = match self.peek() {
            Some(Token::Eq) => BinOp::Eq,
            Some(Token::Ne) => BinOp::Ne,
            Some(Token::Lt) => BinOp::Lt,
            Some(Token::Le) => BinOp::Le,
            Some(Token::Gt) => BinOp::Gt,
            Some(Token::Ge) => BinOp::Ge,
            _ => return Ok(left),
        };
        self.pos += 1;
        Ok(Expr::binary(op, left, self.additive()?))
    }

    fn additive(&mut self) -> Result<Expr> {
        let mut left = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Some(Token::Plus) => BinOp::Add,
                Some(Token::Minus) => BinOp::Sub,
                _ => return Ok(left),
            };
            self.pos += 1;
            left = Expr::binary(op, left, self.multiplicative()?);
        }
    }

    fn multiplicative(&mut self) -> Result<Expr> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(Token::Star) => BinOp::Mul,
                Some(Token::Slash) => BinOp::Div,
                Some(Token::Percent) => BinOp::Mod,
                _ => return Ok(left),
            };
            self.pos += 1;
            left = Expr::binary(op, left, self.unary()?);
        }
    }

    fn unary(&mut self) -> Result<Expr> {
        if self.eat(&Token::Minus) {
            Ok(Expr::Unary { op: UnOp::Neg, expr: Box::new(self.unary()?) })
        } else {
            self.primary()
        }
    }

    fn primary(&mut self) -> Result<Expr> {
        match self.next() {
            Some(Token::Int(i)) => Ok(Expr::Lit(Value::Int(i))),
            Some(Token::Float(x)) => Ok(Expr::Lit(Value::Float(x))),
            Some(Token::Str(s)) => Ok(Expr::Lit(Value::Text(s))),
            Some(Token::LParen) => {
                let e = self.expr()?;
                self.expect(Token::RParen)?;
                Ok(e)
            }
            Some(Token::Ident(s)) => {
                let lower = s.to_ascii_lowercase();
                match lower.as_str() {
                    "true" => return Ok(Expr::Lit(Value::Bool(true))),
                    "false" => return Ok(Expr::Lit(Value::Bool(false))),
                    "null" => return Ok(Expr::Lit(Value::Null)),
                    _ => {}
                }
                if self.peek() == Some(&Token::LParen) {
                    return self.aggregate(&lower);
                }
                if self.eat(&Token::Dot) {
                    return Ok(Expr::Col { table: Some(lower), name: self.ident()? });
                }
                Ok(Expr::Col { table: None, name: lower })
            }
            other => Err(Error::parse(format!("expressão inválida perto de {other:?}"))),
        }
    }

    fn aggregate(&mut self, name: &str) -> Result<Expr> {
        let func = match name {
            "count" => AggFunc::Count,
            "sum" => AggFunc::Sum,
            "avg" => AggFunc::Avg,
            "min" => AggFunc::Min,
            "max" => AggFunc::Max,
            other => return Err(Error::parse(format!("função desconhecida '{other}'"))),
        };
        self.expect(Token::LParen)?;
        if self.peek_kw("DISTINCT") {
            return Err(Error::parse("DISTINCT em agregações não é suportado"));
        }
        let arg = if self.eat(&Token::Star) {
            if func != AggFunc::Count {
                return Err(Error::parse(format!("{name}(*) não existe")));
            }
            None
        } else {
            Some(Box::new(self.expr()?))
        };
        self.expect(Token::RParen)?;
        Ok(Expr::Agg { func, arg })
    }
}
