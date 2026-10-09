//! Esquema de tabelas e sua serialização para o catálogo.

use crate::error::{Error, Result};
use crate::value::DataType;

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub name: String,
    pub ty: DataType,
    pub not_null: bool,
    pub primary_key: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Schema {
    pub columns: Vec<Column>,
}

impl Schema {
    pub fn index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }

    pub fn primary_key(&self) -> Option<usize> {
        self.columns.iter().position(|c| c.primary_key)
    }

    /// "id,INT,1,1;nome,TEXT,0,0"
    pub fn serialize(&self) -> String {
        self.columns
            .iter()
            .map(|c| format!("{},{},{},{}", c.name, c.ty.name(), c.not_null as u8, c.primary_key as u8))
            .collect::<Vec<_>>()
            .join(";")
    }

    pub fn deserialize(s: &str) -> Result<Self> {
        let bad = || Error::storage("esquema corrompido no catálogo");
        let mut columns = Vec::new();
        for part in s.split(';') {
            let f: Vec<&str> = part.split(',').collect();
            if f.len() != 4 {
                return Err(bad());
            }
            columns.push(Column {
                name: f[0].to_string(),
                ty: DataType::parse(f[1]).ok_or_else(bad)?,
                not_null: f[2] == "1",
                primary_key: f[3] == "1",
            });
        }
        Ok(Schema { columns })
    }
}
