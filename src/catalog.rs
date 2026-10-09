//! Metadados em memória: tabelas e índices.
//!
//! Persistência: o catálogo é uma tabela comum (heap na página 1) com dois tipos de
//! linha, então o banco descreve a si mesmo com o mesmo mecanismo dos dados (e as
//! mudanças de DDL passam pelo WAL como qualquer inserção).
//!
//! ```text
//! ("table", nome, raiz_do_heap, esquema)
//! ("index", nome, tabela, coluna, único, raiz_da_btree)
//! ```

use crate::btree::BTree;
use crate::heap::{HeapFile, RowId};
use crate::keyenc::{value_key, value_rid_key};
use crate::lockmgr::TableId;
use crate::schema::Schema;
use crate::value::{Row, Value};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Metadados de um índice secundário ou da chave primária.
pub struct IndexInfo {
    /// Nome do índice (único no banco). O índice da chave primária se chama `pk_<tabela>`.
    pub name: String,
    /// Tabela indexada.
    pub table: String,
    /// Posição da coluna indexada no esquema da tabela.
    pub column: usize,
    /// Se o índice impõe unicidade.
    pub unique: bool,
    /// A B+tree que guarda as entradas.
    pub tree: BTree,
    /// Linha deste índice no heap do catálogo (usada para removê-lo).
    pub catalog_rid: RowId,
}

impl IndexInfo {
    /// Chave da entrada deste índice para uma linha. Índices únicos usam só o valor
    /// (a B-tree rejeita duplicatas); os demais acrescentam o `RowId`. Um `NULL`
    /// nunca viola unicidade, então também recebe o `RowId`.
    pub fn key_for(&self, row: &Row, rid: RowId) -> Vec<u8> {
        let v = &row[self.column];
        if self.unique && *v != Value::Null {
            value_key(v)
        } else {
            value_rid_key(v, rid)
        }
    }
}

/// Metadados e estruturas de uma tabela: esquema, heap e índices.
pub struct TableInfo {
    /// Nome da tabela, em minúsculas.
    pub name: String,
    /// Colunas da tabela.
    pub schema: Schema,
    /// Heap file que guarda as linhas.
    pub heap: HeapFile,
    /// Índices da tabela. Fica atrás de um `RwLock` porque `CREATE INDEX` e `DROP INDEX`
    /// modificam a lista depois que a tabela já foi publicada.
    pub indexes: RwLock<Vec<Arc<IndexInfo>>>,
    /// Linha desta tabela no heap do catálogo.
    pub catalog_rid: RowId,
    /// marcado quando um `DROP TABLE` commita; transações que esperavam o lock revalidam
    pub dropped: AtomicBool,
}

impl TableInfo {
    /// A raiz do heap identifica a tabela (nunca é reaproveitada).
    pub fn id(&self) -> TableId {
        self.heap.root
    }

    /// Cópia da lista de índices (barata: são `Arc`s).
    pub fn indexes(&self) -> Vec<Arc<IndexInfo>> {
        self.indexes.read().clone()
    }

    /// Índices que cobrem a coluna de posição `column`.
    pub fn index_on(&self, column: usize) -> Vec<Arc<IndexInfo>> {
        self.indexes.read().iter().filter(|i| i.column == column).cloned().collect()
    }
}

/// O catálogo em memória: tabelas por nome.
///
/// É uma visão derivada do heap do catálogo, reconstruída na abertura do banco. Mudanças de DDL
/// só são publicadas aqui **depois** do commit durável (`Txn::on_commit`).
#[derive(Default)]
pub struct Catalog {
    tables: RwLock<HashMap<String, Arc<TableInfo>>>,
}

impl Catalog {
    /// Procura uma tabela pelo nome (em minúsculas).
    pub fn get(&self, name: &str) -> Option<Arc<TableInfo>> {
        self.tables.read().get(name).cloned()
    }

    /// Torna uma tabela visível. Substitui qualquer tabela de mesmo nome.
    pub fn publish(&self, table: Arc<TableInfo>) {
        self.tables.write().insert(table.name.clone(), table);
    }

    /// Esquece uma tabela (depois de um `DROP TABLE` commitado).
    pub fn remove(&self, name: &str) {
        self.tables.write().remove(name);
    }

    /// Todas as tabelas, ordenadas pelo nome.
    pub fn all(&self) -> Vec<Arc<TableInfo>> {
        let mut v: Vec<_> = self.tables.read().values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// (tabela, índice) com esse nome, se existir.
    pub fn find_index(&self, name: &str) -> Option<(Arc<TableInfo>, Arc<IndexInfo>)> {
        self.all()
            .into_iter()
            .find_map(|t| t.indexes().into_iter().find(|i| i.name == name).map(|i| (t.clone(), i)))
    }
}
