//! Esquema de tabelas e sua serialização para o catálogo.

use crate::error::{Error, Result};
use crate::value::DataType;

/// Definição de uma coluna.
#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    /// Nome, em minúsculas.
    pub name: String,
    /// Tipo.
    pub ty: DataType,
    /// Rejeita `NULL` (sempre verdadeiro na chave primária).
    pub not_null: bool,
    /// É a chave primária da tabela.
    pub primary_key: bool,
}

/// Esquema de uma tabela: as colunas, na ordem.
#[derive(Debug, Clone, Default)]
pub struct Schema {
    /// As colunas.
    pub columns: Vec<Column>,
}

impl Schema {
    /// Posição da coluna com esse nome.
    pub fn index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }

    /// Posição da coluna que é chave primária, se houver.
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

    /// Lê o texto produzido por [`Schema::serialize`].
    ///
    /// # Errors
    /// `Error::Storage` se o texto está malformado (catálogo corrompido).
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
