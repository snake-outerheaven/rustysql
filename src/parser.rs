//! Parser descendente recursivo: tokens → `Statement`.
//! Identificadores são normalizados para minúsculas.

use crate::ast::*;
use crate::error::{Error, Result};
use crate::lexer::{tokenize, Token};
use crate::schema::Column;
use crate::value::{DataType, Value};

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

struct Parser {
    toks: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos)
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

    fn peek_kw(&self, kw: &str) -> bool {
        matches!(self.peek(), Some(Token::Ident(s)) if s.eq_ignore_ascii_case(kw))
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

    // ---- comandos -------------------------------------------------------

    fn statement(&mut self) -> Result<Statement> {
        if self.eat_kw("CREATE") {
            self.create_table()
        } else if self.eat_kw("DROP") {
            self.expect_kw("TABLE")?;
            Ok(Statement::DropTable { name: self.ident()? })
        } else if self.eat_kw("INSERT") {
            self.insert()
        } else if self.eat_kw("SELECT") {
            self.select()
        } else if self.eat_kw("UPDATE") {
            self.update()
        } else if self.eat_kw("DELETE") {
            self.expect_kw("FROM")?;
            let table = self.ident()?;
            Ok(Statement::Delete { table, filter: self.where_clause()? })
        } else {
            Err(Error::parse(format!("comando desconhecido: {:?}", self.peek())))
        }
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

    fn select(&mut self) -> Result<Statement> {
        let mut items = Vec::new();
        loop {
            items.push(if self.eat(&Token::Star) {
                SelectItem::Star
            } else {
                SelectItem::Expr(self.expr()?)
            });
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        self.expect_kw("FROM")?;
        let table = self.ident()?;
        let filter = self.where_clause()?;
        let mut order_by = None;
        if self.eat_kw("ORDER") {
            self.expect_kw("BY")?;
            let col = self.ident()?;
            let desc = if self.eat_kw("DESC") {
                true
            } else {
                self.eat_kw("ASC");
                false
            };
            order_by = Some((col, desc));
        }
        let mut limit = None;
        if self.eat_kw("LIMIT") {
            match self.next() {
                Some(Token::Int(n)) if n >= 0 => limit = Some(n as usize),
                other => return Err(Error::parse(format!("LIMIT inválido: {other:?}"))),
            }
        }
        Ok(Statement::Select { table, items, filter, order_by, limit })
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

    // ---- expressões (precedência: OR < AND < NOT < comparação < +- < */) --

    fn expr(&mut self) -> Result<Expr> {
        let mut left = self.and_expr()?;
        while self.eat_kw("OR") {
            let right = self.and_expr()?;
            left = Expr::Binary { op: BinOp::Or, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn and_expr(&mut self) -> Result<Expr> {
        let mut left = self.not_expr()?;
        while self.eat_kw("AND") {
            let right = self.not_expr()?;
            left = Expr::Binary { op: BinOp::And, left: Box::new(left), right: Box::new(right) };
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
        let right = self.additive()?;
        Ok(Expr::Binary { op, left: Box::new(left), right: Box::new(right) })
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
            let right = self.multiplicative()?;
            left = Expr::Binary { op, left: Box::new(left), right: Box::new(right) };
        }
    }

    fn multiplicative(&mut self) -> Result<Expr> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(Token::Star) => BinOp::Mul,
                Some(Token::Slash) => BinOp::Div,
                _ => return Ok(left),
            };
            self.pos += 1;
            let right = self.unary()?;
            left = Expr::Binary { op, left: Box::new(left), right: Box::new(right) };
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
            Some(Token::Ident(s)) => Ok(match s.to_ascii_uppercase().as_str() {
                "TRUE" => Expr::Lit(Value::Bool(true)),
                "FALSE" => Expr::Lit(Value::Bool(false)),
                "NULL" => Expr::Lit(Value::Null),
                _ => Expr::Col(s.to_ascii_lowercase()),
            }),
            other => Err(Error::parse(format!("expressão inválida perto de {other:?}"))),
        }
    }
}
