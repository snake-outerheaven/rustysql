//! Árvore sintática dos comandos SQL suportados.

use crate::schema::Column;
use crate::value::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Lit(Value),
    Col(String),
    Unary { op: UnOp, expr: Box<Expr> },
    Binary { op: BinOp, left: Box<Expr>, right: Box<Expr> },
    IsNull { expr: Box<Expr>, negated: bool },
}

#[derive(Debug, Clone)]
pub enum SelectItem {
    Star,
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub enum Statement {
    CreateTable { name: String, columns: Vec<Column> },
    DropTable { name: String },
    Insert { table: String, columns: Option<Vec<String>>, rows: Vec<Vec<Expr>> },
    Select {
        table: String,
        items: Vec<SelectItem>,
        filter: Option<Expr>,
        order_by: Option<(String, bool)>, // (coluna, descendente?)
        limit: Option<usize>,
    },
    Update { table: String, sets: Vec<(String, Expr)>, filter: Option<Expr> },
    Delete { table: String, filter: Option<Expr> },
}
