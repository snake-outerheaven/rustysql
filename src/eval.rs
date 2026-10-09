//! Expressões: `Expr` (sintaxe) → `BExpr` (ligada às posições das colunas) → valor.
//! A resolução de nomes acontece uma vez, no `bind`; por linha só se indexa o vetor.
//! Lógica de três valores para NULL.

use crate::ast::{BinOp, Expr, UnOp};
use crate::error::{Error, Result};
use crate::schema::Schema;
use crate::value::Value;
use std::cmp::Ordering;

/// Uma coluna visível numa expressão: a tabela (ou alias) de origem e o nome.
#[derive(Debug, Clone, PartialEq)]
pub struct ScopeCol {
    /// Tabela ou alias de origem. Vazio nas colunas sintéticas (`#aggN`) e nas de saída de um `Project`.
    pub table: String,
    /// Nome da coluna.
    pub name: String,
}

/// Colunas visíveis para uma expressão, na ordem em que aparecem na linha.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    /// As colunas, na ordem em que aparecem em cada linha.
    pub cols: Vec<ScopeCol>,
}

impl Scope {
    /// Escopo de uma tabela inteira, com as colunas qualificadas pelo `alias`.
    pub fn for_table(alias: &str, schema: &Schema) -> Scope {
        Scope {
            cols: schema
                .columns
                .iter()
                .map(|c| ScopeCol { table: alias.to_string(), name: c.name.clone() })
                .collect(),
        }
    }

    /// Junta dois escopos (o resultado de um join: colunas da esquerda e depois da direita).
    pub fn concat(&self, other: &Scope) -> Scope {
        Scope { cols: self.cols.iter().chain(&other.cols).cloned().collect() }
    }

    /// Descobre a posição de uma coluna. Se `table` é `None`, o nome precisa ser único no escopo.
    ///
    /// # Errors
    /// `Error::Exec` se a coluna não existe ou se o nome é ambíguo.
    pub fn resolve(&self, table: Option<&str>, name: &str) -> Result<usize> {
        let found: Vec<usize> = self
            .cols
            .iter()
            .enumerate()
            .filter(|(_, c)| c.name == name && table.is_none_or(|t| c.table == t))
            .map(|(i, _)| i)
            .collect();
        match found.len() {
            1 => Ok(found[0]),
            0 => Err(Error::exec(match table {
                Some(t) => format!("coluna desconhecida '{t}.{name}'"),
                None => format!("coluna desconhecida '{name}'"),
            })),
            _ => Err(Error::exec(format!("coluna '{name}' é ambígua; qualifique com o nome da tabela"))),
        }
    }
}

/// Expressão **ligada** a um [`Scope`]: as colunas já são posições (`Col(i)`), então avaliar
/// uma linha é indexar um vetor, sem busca por nome.
///
/// É produzida por [`bind`] e avaliada por [`BExpr::eval`].
#[derive(Debug, Clone)]
pub enum BExpr {
    /// Valor constante.
    Lit(Value),
    /// Posição da coluna na linha.
    Col(usize),
    /// Operação unária.
    Unary {
        /// O operador.
        op: UnOp,
        /// O operando.
        expr: Box<BExpr>,
    },
    /// Operação binária.
    Binary {
        /// O operador.
        op: BinOp,
        /// Operando da esquerda.
        left: Box<BExpr>,
        /// Operando da direita.
        right: Box<BExpr>,
    },
    /// `IS [NOT] NULL`.
    IsNull {
        /// Expressão testada.
        expr: Box<BExpr>,
        /// `true` para `IS NOT NULL`.
        negated: bool,
    },
    /// `[NOT] LIKE`.
    Like {
        /// Texto a casar.
        expr: Box<BExpr>,
        /// Padrão (`%` e `_`).
        pattern: Box<BExpr>,
        /// `true` para `NOT LIKE`.
        negated: bool,
    },
}

/// Liga uma [`Expr`] a um escopo, resolvendo os nomes de coluna.
///
/// # Errors
/// `Error::Exec` se alguma coluna é desconhecida ou ambígua, ou se a expressão contém uma
/// função de agregação (elas são extraídas pelo planejador antes do `bind`).
pub fn bind(e: &Expr, scope: &Scope) -> Result<BExpr> {
    Ok(match e {
        Expr::Lit(v) => BExpr::Lit(v.clone()),
        Expr::Col { table, name } => BExpr::Col(scope.resolve(table.as_deref(), name)?),
        Expr::Unary { op, expr } => BExpr::Unary { op: *op, expr: Box::new(bind(expr, scope)?) },
        Expr::Binary { op, left, right } => BExpr::Binary {
            op: *op,
            left: Box::new(bind(left, scope)?),
            right: Box::new(bind(right, scope)?),
        },
        Expr::IsNull { expr, negated } => {
            BExpr::IsNull { expr: Box::new(bind(expr, scope)?), negated: *negated }
        }
        Expr::Like { expr, pattern, negated } => BExpr::Like {
            expr: Box::new(bind(expr, scope)?),
            pattern: Box::new(bind(pattern, scope)?),
            negated: *negated,
        },
        Expr::Agg { .. } => {
            return Err(Error::exec("função de agregação não é permitida neste contexto"));
        }
    })
}

/// Valor de uma expressão sem colunas (literais e aritmética sobre eles), se for constante.
pub fn const_value(e: &Expr) -> Option<Value> {
    let mut cols = Vec::new();
    e.columns(&mut cols);
    if !cols.is_empty() || e.contains_agg() {
        return None;
    }
    bind(e, &Scope::default()).ok()?.eval(&[]).ok()
}

impl BExpr {
    /// Avalia a expressão sobre uma linha, com lógica de três valores para `NULL`.
    ///
    /// # Errors
    /// `Error::Exec` em divisão por zero, overflow inteiro ou operandos de tipos incompatíveis.
    pub fn eval(&self, row: &[Value]) -> Result<Value> {
        match self {
            BExpr::Lit(v) => Ok(v.clone()),
            BExpr::Col(i) => Ok(row[*i].clone()),
            BExpr::IsNull { expr, negated } => {
                Ok(Value::Bool((expr.eval(row)? == Value::Null) != *negated))
            }
            BExpr::Unary { op, expr } => match (op, expr.eval(row)?) {
                (_, Value::Null) => Ok(Value::Null),
                (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                (UnOp::Neg, Value::Int(i)) => {
                    i.checked_neg().map(Value::Int).ok_or_else(|| Error::exec("overflow inteiro"))
                }
                (UnOp::Neg, Value::Float(x)) => Ok(Value::Float(-x)),
                (op, v) => Err(Error::exec(format!("operador {op:?} inválido para {v:?}"))),
            },
            BExpr::Like { expr, pattern, negated } => match (expr.eval(row)?, pattern.eval(row)?) {
                (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
                (Value::Text(t), Value::Text(p)) => Ok(Value::Bool(like(&t, &p) != *negated)),
                (a, b) => Err(Error::exec(format!("LIKE exige texto, recebeu {a:?} e {b:?}"))),
            },
            BExpr::Binary { op, left, right } => {
                let a = left.eval(row)?;
                let b = right.eval(row)?;
                match op {
                    BinOp::And | BinOp::Or => logic(*op, &a, &b),
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                        compare(*op, &a, &b)
                    }
                    _ => arith(*op, a, b),
                }
            }
        }
    }

    /// Um filtro só aceita a linha quando o resultado é exatamente TRUE.
    pub fn test(&self, row: &[Value]) -> Result<bool> {
        match self.eval(row)? {
            Value::Bool(b) => Ok(b),
            Value::Null => Ok(false),
            v => Err(Error::exec(format!("a condição precisa ser booleana, veio {v:?}"))),
        }
    }
}

/// `%` casa qualquer sequência, `_` casa um caractere.
fn like(text: &str, pattern: &str) -> bool {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    let (mut ti, mut pi) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && p[pi] == '%' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if pi < p.len() && (p[pi] == '_' || p[pi] == t[ti]) {
            ti += 1;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '%' {
        pi += 1;
    }
    pi == p.len()
}

fn tri(v: &Value) -> Result<Option<bool>> {
    match v {
        Value::Bool(b) => Ok(Some(*b)),
        Value::Null => Ok(None),
        v => Err(Error::exec(format!("esperava booleano, veio {v:?}"))),
    }
}

fn logic(op: BinOp, a: &Value, b: &Value) -> Result<Value> {
    let (a, b) = (tri(a)?, tri(b)?);
    let r = match op {
        BinOp::And => match (a, b) {
            (Some(false), _) | (_, Some(false)) => Some(false),
            (Some(true), Some(true)) => Some(true),
            _ => None,
        },
        _ => match (a, b) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        },
    };
    Ok(r.map_or(Value::Null, Value::Bool))
}

fn compare(op: BinOp, a: &Value, b: &Value) -> Result<Value> {
    if *a == Value::Null || *b == Value::Null {
        return Ok(Value::Null);
    }
    let ord = a
        .sql_cmp(b)
        .ok_or_else(|| Error::exec(format!("não é possível comparar {a:?} com {b:?}")))?;
    Ok(Value::Bool(match op {
        BinOp::Eq => ord == Ordering::Equal,
        BinOp::Ne => ord != Ordering::Equal,
        BinOp::Lt => ord == Ordering::Less,
        BinOp::Le => ord != Ordering::Greater,
        BinOp::Gt => ord == Ordering::Greater,
        _ => ord != Ordering::Less,
    }))
}

fn arith(op: BinOp, a: Value, b: Value) -> Result<Value> {
    use Value::*;
    let overflow = || Error::exec("overflow inteiro");
    match (a, b) {
        (Null, _) | (_, Null) => Ok(Null),
        (Int(x), Int(y)) => match op {
            BinOp::Add => x.checked_add(y).map(Int).ok_or_else(overflow),
            BinOp::Sub => x.checked_sub(y).map(Int).ok_or_else(overflow),
            BinOp::Mul => x.checked_mul(y).map(Int).ok_or_else(overflow),
            _ if y == 0 => Err(Error::exec("divisão por zero")),
            BinOp::Mod => x.checked_rem(y).map(Int).ok_or_else(overflow),
            _ => x.checked_div(y).map(Int).ok_or_else(overflow),
        },
        (Text(x), Text(y)) if op == BinOp::Add => Ok(Text(x + &y)),
        (a @ (Int(_) | Float(_)), b @ (Int(_) | Float(_))) => {
            let f = |v: Value| match v {
                Int(i) => i as f64,
                Float(x) => x,
                _ => unreachable!(),
            };
            let (x, y) = (f(a), f(b));
            Ok(Float(match op {
                BinOp::Add => x + y,
                BinOp::Sub => x - y,
                BinOp::Mul => x * y,
                BinOp::Mod => x % y,
                _ => x / y,
            }))
        }
        (a, b) => Err(Error::exec(format!("{op:?} inválido entre {a:?} e {b:?}"))),
    }
}
