//! Tipos de dados, valores e a codificação binária de linhas.

use crate::error::{Error, Result};
use std::cmp::Ordering;
use std::fmt;

/// Tipos de coluna suportados.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    /// Inteiro de 64 bits com sinal (`INT`, `INTEGER`, `BIGINT`).
    Int,
    /// Ponto flutuante de 64 bits (`FLOAT`, `REAL`, `DOUBLE`).
    Float,
    /// Texto UTF-8 (`TEXT`, `VARCHAR`, `STRING`). O tamanho em `VARCHAR(n)` é ignorado.
    Text,
    /// Booleano (`BOOL`, `BOOLEAN`).
    Bool,
}

impl DataType {
    /// Nome canônico em maiúsculas.
    pub fn name(self) -> &'static str {
        match self {
            DataType::Int => "INT",
            DataType::Float => "FLOAT",
            DataType::Text => "TEXT",
            DataType::Bool => "BOOL",
        }
    }

    /// Interpreta o nome de um tipo (sem diferenciar maiúsculas); `None` se desconhecido.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "INT" | "INTEGER" | "BIGINT" => Some(DataType::Int),
            "FLOAT" | "REAL" | "DOUBLE" => Some(DataType::Float),
            "TEXT" | "VARCHAR" | "STRING" => Some(DataType::Text),
            "BOOL" | "BOOLEAN" => Some(DataType::Bool),
            _ => None,
        }
    }
}

/// Um valor SQL.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Ausência de valor. Em comparações dá sempre "desconhecido", nunca verdadeiro.
    Null,
    /// Inteiro.
    Int(i64),
    /// Decimal.
    Float(f64),
    /// Texto.
    Text(String),
    /// Booleano.
    Bool(bool),
}

/// Uma linha: um valor por coluna, na ordem do esquema.
pub type Row = Vec<Value>;

impl Value {
    /// Comparação SQL: `None` quando envolve NULL ou tipos incompatíveis.
    pub fn sql_cmp(&self, other: &Value) -> Option<Ordering> {
        use Value::*;
        match (self, other) {
            (Null, _) | (_, Null) => None,
            (Int(a), Int(b)) => Some(a.cmp(b)),
            (Int(a), Float(b)) => (*a as f64).partial_cmp(b),
            (Float(a), Int(b)) => a.partial_cmp(&(*b as f64)),
            (Float(a), Float(b)) => a.partial_cmp(b),
            (Text(a), Text(b)) => Some(a.cmp(b)),
            (Bool(a), Bool(b)) => Some(a.cmp(b)),
            _ => None,
        }
    }

    /// Ordem total para ORDER BY (NULL vem primeiro).
    pub fn total_cmp(&self, other: &Value) -> Ordering {
        match (self, other) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Null, _) => Ordering::Less,
            (_, Value::Null) => Ordering::Greater,
            _ => self.sql_cmp(other).unwrap_or(Ordering::Equal),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, "NULL"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(x) => write!(f, "{x}"),
            Value::Text(s) => write!(f, "{s}"),
            Value::Bool(b) => write!(f, "{b}"),
        }
    }
}

// ---- codificação binária: tag (1 byte) + payload ----------------------

const TAG_NULL: u8 = 0;
const TAG_INT: u8 = 1;
const TAG_FLOAT: u8 = 2;
const TAG_TEXT: u8 = 3;
const TAG_BOOL: u8 = 4;

/// Serializa uma linha: para cada valor, um byte de tag seguido do conteúdo (`Int` e `Float`
/// com 8 bytes little-endian; `Text` com tamanho `u32` e bytes UTF-8; `Bool` com 1 byte).
pub fn encode_row(row: &[Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in row {
        match v {
            Value::Null => out.push(TAG_NULL),
            Value::Int(i) => {
                out.push(TAG_INT);
                out.extend_from_slice(&i.to_le_bytes());
            }
            Value::Float(x) => {
                out.push(TAG_FLOAT);
                out.extend_from_slice(&x.to_le_bytes());
            }
            Value::Text(s) => {
                out.push(TAG_TEXT);
                out.extend_from_slice(&(s.len() as u32).to_le_bytes());
                out.extend_from_slice(s.as_bytes());
            }
            Value::Bool(b) => {
                out.push(TAG_BOOL);
                out.push(*b as u8);
            }
        }
    }
    out
}

/// Lê uma linha serializada por [`encode_row`].
///
/// # Errors
/// `Error::Storage` se o buffer está truncado, tem uma tag desconhecida ou texto inválido.
pub fn decode_row(mut buf: &[u8]) -> Result<Row> {
    fn take<'a>(buf: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
        if buf.len() < n {
            return Err(Error::storage("registro truncado"));
        }
        let (head, tail) = buf.split_at(n);
        *buf = tail;
        Ok(head)
    }

    let mut row = Vec::new();
    while !buf.is_empty() {
        let tag = take(&mut buf, 1)?[0];
        row.push(match tag {
            TAG_NULL => Value::Null,
            TAG_INT => Value::Int(i64::from_le_bytes(take(&mut buf, 8)?.try_into().unwrap())),
            TAG_FLOAT => Value::Float(f64::from_le_bytes(take(&mut buf, 8)?.try_into().unwrap())),
            TAG_TEXT => {
                let len = u32::from_le_bytes(take(&mut buf, 4)?.try_into().unwrap()) as usize;
                let bytes = take(&mut buf, len)?;
                Value::Text(
                    String::from_utf8(bytes.to_vec())
                        .map_err(|_| Error::storage("texto UTF-8 inválido"))?,
                )
            }
            TAG_BOOL => Value::Bool(take(&mut buf, 1)?[0] != 0),
            t => return Err(Error::storage(format!("tag desconhecida {t}"))),
        });
    }
    Ok(row)
}
