//! Transação: acumula (1) as operações de WAL que serão gravadas atomicamente no
//! commit e (2) o log de undo em memória para o rollback.
//!
//! Como o WAL só é escrito no commit e as páginas sujas ficam presas no buffer pool
//! (no-steal), desfazer uma transação é só aplicar o undo em memória, em ordem
//! inversa, e descartar as operações de WAL. A única exceção são as operações
//! *estruturais* (página nova + encadeamento), que sobrevivem ao rollback: a página
//! continua existindo e transações futuras vão usá-la.

use crate::btree::BTree;
use crate::error::Result;
use crate::heap::{HeapFile, RowId};
use crate::lockmgr::TxnId;
use crate::sched::{Permit, Priority};
use crate::wal::WalOp;
use parking_lot::{ArcMutexGuard, ArcRwLockReadGuard, RawMutex, RawRwLock};

/// Uma ação de desfazer, guardada em ordem; o rollback as aplica de trás para frente.
pub enum Undo {
    /// Desfaz uma inserção no heap: apaga o registro.
    HeapInsert {
        /// Heap afetado.
        heap: HeapFile,
        /// Registro inserido.
        rid: RowId,
    },
    /// Desfaz uma remoção no heap: recoloca o registro **no mesmo** `RowId`.
    HeapDelete {
        /// Heap afetado.
        heap: HeapFile,
        /// Onde o registro estava.
        rid: RowId,
        /// Conteúdo apagado.
        data: Vec<u8>,
    },
    /// Desfaz uma inserção no índice: remove a chave.
    IndexInsert {
        /// Árvore afetada.
        tree: BTree,
        /// Chave inserida.
        key: Vec<u8>,
    },
    /// Desfaz uma remoção no índice: reinsere a entrada.
    IndexDelete {
        /// Árvore afetada.
        tree: BTree,
        /// Chave removida.
        key: Vec<u8>,
        /// Registro para o qual ela apontava.
        rid: RowId,
    },
}

/// Ponto de salvamento: permite desfazer só o último comando dentro de uma transação.
#[derive(Clone, Copy)]
pub struct Mark {
    undo: usize,
    wal: usize,
}

/// Estado de uma transação em andamento.
///
/// Acumula (1) as operações de WAL que serão gravadas atomicamente no commit e (2) o log de
/// undo em memória para o rollback. Os recursos que ela segura (portão de checkpoint, guard de
/// DDL, vaga de execução, locks) são liberados quando ela é descartada ou terminada.
pub struct Txn {
    /// Identificador.
    pub id: TxnId,
    /// Aberta com `BEGIN` (DDL não é permitido dentro dela).
    pub explicit: bool,
    /// prioridade da sessão que abriu a transação: protege contra ser escolhida como vítima
    pub priority: Priority,
    /// Operações a gravar no WAL no commit, em ordem.
    pub wal: Vec<WalOp>,
    /// Ações de desfazer, em ordem de execução.
    pub undo: Vec<Undo>,
    /// executado depois do commit durável (publica mudanças de catálogo)
    pub on_commit: Vec<Box<dyn FnOnce() + Send>>,
    /// enquanto a transação existe, nenhum checkpoint pode rodar
    pub _gate: ArcRwLockReadGuard<RawRwLock, ()>,
    /// DDL é serializado: um só por vez
    pub ddl_guard: Option<ArcMutexGuard<RawMutex, ()>>,
    /// vaga de execução do comando em andamento (devolvida ao terminar o comando)
    pub permit: Option<Permit>,
    /// já pegou algum lock de tabela (os próximos comandos entram na frente na fila)
    pub locked: bool,
}

impl Mark {
    /// ponto zero: desfaz a transação inteira
    pub fn start() -> Mark {
        Mark { undo: 0, wal: 0 }
    }
}

impl Txn {
    /// Marca o ponto atual, para desfazer só o que vier depois (rollback de um comando).
    pub fn mark(&self) -> Mark {
        Mark { undo: self.undo.len(), wal: self.wal.len() }
    }

    /// Desfaz, de trás para frente, tudo o que foi feito depois de `mark`.
    ///
    /// Descarta as operações de WAL desfeitas, mas mantém as *estruturais* (página nova e seu
    /// encadeamento): a página continua alocada e transações futuras vão escrever nela.
    ///
    /// # Errors
    /// Devolve o primeiro erro de undo, mas tenta aplicar todos.
    pub fn rollback_to(&mut self, mark: Mark) -> Result<()> {
        let mut first_err = None;
        while self.undo.len() > mark.undo {
            let step = match self.undo.pop().unwrap() {
                Undo::HeapInsert { heap, rid } => heap.delete(rid, &mut Vec::new()).map(|_| ()),
                Undo::HeapDelete { heap, rid, data } => heap.put_at(rid, &data, &mut Vec::new()),
                Undo::IndexInsert { tree, key } => tree.delete(&key).map(|_| ()),
                Undo::IndexDelete { tree, key, rid } => tree.insert(&key, rid).map(|_| ()),
            };
            if let Err(e) = step {
                first_err.get_or_insert(e);
            }
        }
        // descarta as operações desfeitas, mas mantém as estruturais
        let tail = self.wal.split_off(mark.wal.min(self.wal.len()));
        self.wal.extend(tail.into_iter().filter(WalOp::is_structural));
        first_err.map_or(Ok(()), Err)
    }
}
