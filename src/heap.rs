//! Heap file: registros em *slotted pages*, agora via buffer pool.
//!
//! Layout da página:
//! ```text
//! [0..4]  next page (0 = fim)   [4..6] nº de slots   [6..8] free_end
//! [8..]   slots: (offset u16, len u16) × n  →  ...espaço livre...  ← registros
//! ```
//! `offset == 0` marca slot livre; o `RowId` (página, slot) é estável porque o
//! slot nunca muda de índice, mesmo quando a página é compactada.

use crate::bufferpool::BufferPool;
use crate::disk::{PageBuf, PageId, PAGE_SIZE};
use crate::error::{Error, Result};
use crate::wal::WalOp;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

const HEADER: usize = 8;
const SLOT: usize = 4;
/// Maior registro que cabe numa página (página menos cabeçalho e um slot).
pub const MAX_RECORD: usize = PAGE_SIZE - HEADER - SLOT;

/// Endereço físico de uma linha: página e slot. É estável durante a vida da linha
/// (a compactação de uma página mexe nos offsets, nunca nos índices dos slots), por isso os
/// índices apontam para ele.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RowId {
    /// Página do heap.
    pub page: PageId,
    /// Índice do slot dentro da página.
    pub slot: u16,
}

/// Operações puras sobre o buffer de uma página.
mod page {
    use super::*;

    fn rd(p: &PageBuf, i: usize) -> usize {
        u16::from_le_bytes([p[i], p[i + 1]]) as usize
    }
    fn wr(p: &mut PageBuf, i: usize, v: usize) {
        p[i..i + 2].copy_from_slice(&(v as u16).to_le_bytes());
    }

    pub fn init(p: &mut PageBuf) {
        p.fill(0);
        wr(p, 6, PAGE_SIZE);
    }
    pub fn next(p: &PageBuf) -> PageId {
        u32::from_le_bytes(p[0..4].try_into().unwrap())
    }
    pub fn set_next(p: &mut PageBuf, id: PageId) {
        p[0..4].copy_from_slice(&id.to_le_bytes());
    }
    pub fn nslots(p: &PageBuf) -> usize {
        rd(p, 4)
    }
    fn slot(p: &PageBuf, i: usize) -> (usize, usize) {
        (rd(p, HEADER + i * SLOT), rd(p, HEADER + i * SLOT + 2))
    }
    fn set_slot(p: &mut PageBuf, i: usize, off: usize, len: usize) {
        wr(p, HEADER + i * SLOT, off);
        wr(p, HEADER + i * SLOT + 2, len);
    }

    pub fn get(p: &PageBuf, i: usize) -> Option<&[u8]> {
        if i >= nslots(p) {
            return None;
        }
        let (off, len) = slot(p, i);
        (off != 0).then(|| &p[off..off + len])
    }

    fn live_bytes(p: &PageBuf) -> usize {
        (0..nslots(p)).map(|i| slot(p, i)).filter(|s| s.0 != 0).map(|s| s.1).sum()
    }

    /// Reescreve os registros vivos colados no fim da página, recuperando o espaço
    /// deixado por registros apagados. Os índices de slot não mudam.
    fn compact(p: &mut PageBuf) {
        let live: Vec<(usize, Vec<u8>)> = (0..nslots(p))
            .filter_map(|i| get(p, i).map(|d| (i, d.to_vec())))
            .collect();
        let mut end = PAGE_SIZE;
        for (i, data) in live {
            end -= data.len();
            p[end..end + data.len()].copy_from_slice(&data);
            set_slot(p, i, end, data.len());
        }
        wr(p, 6, end);
    }

    /// Há espaço para `len` bytes, acrescentando `new_slots` entradas ao array de slots?
    fn has_room(p: &PageBuf, len: usize, new_slots: usize) -> bool {
        let slots_end = HEADER + (nslots(p) + new_slots) * SLOT;
        PAGE_SIZE.saturating_sub(slots_end + live_bytes(p)) >= len
    }

    /// Reserva `len` bytes (compactando se preciso) e devolve o offset.
    fn reserve(p: &mut PageBuf, len: usize, new_slots: usize) -> Option<usize> {
        if !has_room(p, len, new_slots) {
            return None;
        }
        let slots_end = HEADER + (nslots(p) + new_slots) * SLOT;
        if rd(p, 6).saturating_sub(slots_end) < len {
            compact(p);
        }
        let off = rd(p, 6) - len;
        wr(p, 6, off);
        Some(off)
    }

    pub fn can_insert(p: &PageBuf, len: usize) -> bool {
        let reuse = (0..nslots(p)).any(|i| slot(p, i).0 == 0);
        has_room(p, len, if reuse { 0 } else { 1 })
    }

    pub fn insert(p: &mut PageBuf, data: &[u8]) -> Option<u16> {
        let n = nslots(p);
        let reuse = (0..n).find(|&i| slot(p, i).0 == 0);
        let off = reserve(p, data.len(), if reuse.is_some() { 0 } else { 1 })?;
        p[off..off + data.len()].copy_from_slice(data);
        let i = match reuse {
            Some(i) => i,
            None => {
                wr(p, 4, n + 1);
                n
            }
        };
        set_slot(p, i, off, data.len());
        Some(i as u16)
    }

    /// Coloca `data` exatamente no slot `i` (redo da recuperação e undo de um delete).
    /// Idempotente: se o slot já tem esse conteúdo, não faz nada.
    pub fn put_at(p: &mut PageBuf, i: usize, data: &[u8]) -> bool {
        let n = nslots(p);
        if i < n
            && let Some(cur) = get(p, i)
        {
            if cur == data {
                return true;
            }
            set_slot(p, i, 0, 0);
        }
        let new_slots = (i + 1).saturating_sub(n);
        let Some(off) = reserve(p, data.len(), new_slots) else { return false };
        if new_slots > 0 {
            for j in n..=i {
                set_slot(p, j, 0, 0);
            }
            wr(p, 4, i + 1);
        }
        p[off..off + data.len()].copy_from_slice(data);
        set_slot(p, i, off, data.len());
        true
    }

    pub fn delete(p: &mut PageBuf, i: usize) -> Option<Vec<u8>> {
        let data = get(p, i)?.to_vec();
        set_slot(p, i, 0, 0);
        Some(data)
    }
}

/// Uma tabela: lista encadeada de páginas a partir de `root`.
/// `Clone` é barato (o pool é um `Arc`, e o `hint` é compartilhado).
#[derive(Clone)]
pub struct HeapFile {
    pool: Arc<BufferPool>,
    /// Primeira página da cadeia. Identifica a tabela e nunca muda.
    pub root: PageId,
    /// primeira página que pode ter espaço livre: evita percorrer a cadeia toda
    hint: Arc<AtomicU32>,
}

impl HeapFile {
    /// Abre um heap existente a partir da página raiz guardada no catálogo.
    pub fn open(pool: Arc<BufferPool>, root: PageId) -> HeapFile {
        HeapFile { pool, root, hint: Arc::new(AtomicU32::new(root)) }
    }

    /// Cria um heap com uma página vazia e registra a formatação em `log`.
    pub fn create(pool: &Arc<BufferPool>, log: &mut Vec<WalOp>) -> Result<HeapFile> {
        let frame = pool.new_page()?;
        page::init(&mut frame.write());
        log.push(WalOp::HeapFormat { page: frame.id() });
        Ok(HeapFile::open(pool.clone(), frame.id()))
    }

    /// Insere um registro e devolve o seu endereço. Registra a operação em `log`.
    ///
    /// Começa pela página `hint` (a primeira que pode ter espaço) e segue a cadeia; se todas
    /// estão cheias, aloca e encadeia uma página nova.
    ///
    /// # Errors
    /// `Error::Exec` se o registro é vazio ou maior que [`MAX_RECORD`]; falha também se o pool
    /// não comporta mais páginas.
    pub fn insert(&self, data: &[u8], log: &mut Vec<WalOp>) -> Result<RowId> {
        if data.is_empty() || data.len() > MAX_RECORD {
            return Err(Error::exec(format!("registro de {} bytes não cabe numa página", data.len())));
        }
        let mut id = self.hint.load(Ordering::SeqCst);
        loop {
            let frame = self.pool.fetch(id)?;
            let mut g = frame.write();
            if page::can_insert(&g, data.len()) {
                let slot = page::insert(&mut g, data).expect("can_insert garantiu espaço");
                self.hint.store(id, Ordering::SeqCst);
                log.push(WalOp::HeapInsert { page: id, slot, data: data.to_vec() });
                return Ok(RowId { page: id, slot });
            }
            let next = page::next(&g);
            if next != 0 {
                id = next;
                continue;
            }
            let new = self.pool.new_page()?;
            page::init(&mut new.write());
            page::set_next(&mut g, new.id());
            log.push(WalOp::HeapFormat { page: new.id() });
            log.push(WalOp::HeapLink { prev: id, next: new.id() });
            id = new.id();
        }
    }

    /// Lê um registro; `None` se o slot está livre.
    pub fn get(&self, rid: RowId) -> Result<Option<Vec<u8>>> {
        let frame = self.pool.fetch(rid.page)?;
        let g = frame.read();
        Ok(page::get(&g, rid.slot as usize).map(<[u8]>::to_vec))
    }

    /// Apaga e devolve o conteúdo antigo (para o undo).
    pub fn delete(&self, rid: RowId, log: &mut Vec<WalOp>) -> Result<Vec<u8>> {
        let frame = self.pool.fetch(rid.page)?;
        let mut g = frame.write();
        if page::get(&g, rid.slot as usize).is_none() {
            return Err(Error::storage(format!("registro {rid:?} não existe")));
        }
        let old = page::delete(&mut g, rid.slot as usize).unwrap();
        self.hint.fetch_min(rid.page, Ordering::SeqCst);
        log.push(WalOp::HeapDelete { page: rid.page, slot: rid.slot });
        Ok(old)
    }

    /// Recoloca um registro no mesmo `RowId` (desfaz um delete).
    pub fn put_at(&self, rid: RowId, data: &[u8], log: &mut Vec<WalOp>) -> Result<()> {
        let frame = self.pool.fetch(rid.page)?;
        let mut g = frame.write();
        if !page::put_at(&mut g, rid.slot as usize, data) {
            return Err(Error::storage(format!("sem espaço para restaurar {rid:?}")));
        }
        log.push(WalOp::HeapInsert { page: rid.page, slot: rid.slot, data: data.to_vec() });
        Ok(())
    }

    /// Varredura preguiçosa de todos os registros, uma página por vez.
    pub fn scan(&self) -> HeapScan {
        HeapScan { pool: self.pool.clone(), next_page: self.root, buf: VecDeque::new() }
    }
}

/// Varredura preguiçosa: lê uma página por vez, sem segurar latch entre páginas.
pub struct HeapScan {
    pool: Arc<BufferPool>,
    next_page: PageId,
    buf: VecDeque<(RowId, Vec<u8>)>,
}

impl Iterator for HeapScan {
    type Item = Result<(RowId, Vec<u8>)>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = self.buf.pop_front() {
                return Some(Ok(item));
            }
            if self.next_page == 0 {
                return None;
            }
            let id = self.next_page;
            let frame = match self.pool.fetch(id) {
                Ok(f) => f,
                Err(e) => {
                    self.next_page = 0;
                    return Some(Err(e));
                }
            };
            let g = frame.read();
            for i in 0..page::nslots(&g) {
                if let Some(d) = page::get(&g, i) {
                    self.buf.push_back((RowId { page: id, slot: i as u16 }, d.to_vec()));
                }
            }
            self.next_page = page::next(&g);
        }
    }
}

/// Reaplica uma operação do WAL (recuperação). Todas são idempotentes, então
/// reaplicar sobre páginas parcialmente gravadas por um checkpoint interrompido
/// converge para o mesmo estado final.
pub fn apply_op(pool: &BufferPool, op: &WalOp) -> Result<()> {
    match op {
        WalOp::HeapFormat { page: id } => page::init(&mut pool.ensure_page(*id)?.write()),
        WalOp::HeapLink { prev, next } => {
            pool.ensure_page(*next)?;
            page::set_next(&mut pool.ensure_page(*prev)?.write(), *next);
        }
        WalOp::HeapInsert { page: id, slot, data } => {
            let frame = pool.ensure_page(*id)?;
            let mut g = frame.write();
            if !page::put_at(&mut g, *slot as usize, data) {
                return Err(Error::storage(format!("WAL: sem espaço na página {id} no redo")));
            }
        }
        WalOp::HeapDelete { page: id, slot } => {
            let frame = pool.ensure_page(*id)?;
            let mut g = frame.write();
            if page::get(&g, *slot as usize).is_some() {
                page::delete(&mut g, *slot as usize);
            }
        }
    }
    Ok(())
}
