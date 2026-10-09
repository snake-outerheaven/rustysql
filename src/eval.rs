//! Avaliação de expressões sobre uma linha, com lógica de três valores (NULL).

use crate::ast::{BinOp, Expr, UnOp};
use crate::error::{Error, Result};
use crate::schema::Schema;
use crate::value::Value;
use std::cmp::Ordering;

pub fn eval(e: &Expr, schema: &Schema, row: &[Value]) -> Result<Value> {
    match e {
        Expr::Lit(v) => Ok(v.clone()),
        Expr::Col(name) => {
            let i = schema
                .index(name)
                .ok_or_else(|| Error::exec(format!("coluna desconhecida '{name}'")))?;
            Ok(row[i].clone())
        }
        Expr::IsNull { expr, negated } => {
            let is_null = eval(expr, schema, row)? == Value::Null;
            Ok(Value::Bool(is_null != *negated))
        }
        Expr::Unary { op, expr } => match (op, eval(expr, schema, row)?) {
            (_, Value::Null) => Ok(Value::Null),
            (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
            (UnOp::Neg, Value::Int(i)) => i
                .checked_neg()
                .map(Value::Int)
                .ok_or_else(|| Error::exec("overflow inteiro")),
            (UnOp::Neg, Value::Float(x)) => Ok(Value::Float(-x)),
            (op, v) => Err(Error::exec(format!("operador {op:?} inválido para {v:?}"))),
        },
        Expr::Binary { op, left, right } => {
            let a = eval(left, schema, row)?;
            let b = eval(right, schema, row)?;
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
pub fn matches(filter: &Option<Expr>, schema: &Schema, row: &[Value]) -> Result<bool> {
    match filter {
        None => Ok(true),
        Some(f) => match eval(f, schema, row)? {
            Value::Bool(b) => Ok(b),
            Value::Null => Ok(false),
            v => Err(Error::exec(format!("WHERE precisa ser booleano, veio {v:?}"))),
        },
    }
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
                _ => x / y,
            }))
        }
        (a, b) => Err(Error::exec(format!("{op:?} inválido entre {a:?} e {b:?}"))),
    }
}
