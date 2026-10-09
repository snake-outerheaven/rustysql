//! Heap file com *slotted pages*. Uma tabela é uma lista encadeada de páginas.
//!
//! Layout da página:
//! ```text
//! [0..4]  next page (0 = fim)   [4..6] nº de slots   [6..8] free_end
//! [8..]   slots: (offset u16, len u16) × n  →  ...espaço livre...  ← registros
//! ```
//! Registros crescem do fim da página para o início. `offset == 0` = slot apagado.

use crate::error::{Error, Result};
use crate::pager::{Page, PageId, Pager, PAGE_SIZE};

const HEADER: usize = 8;
const SLOT: usize = 4;
pub const MAX_RECORD: usize = PAGE_SIZE - HEADER - SLOT;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RowId {
    pub page: PageId,
    pub slot: u16,
}

fn u16_at(p: &Page, i: usize) -> usize {
    u16::from_le_bytes([p[i], p[i + 1]]) as usize
}
fn set_u16(p: &mut Page, i: usize, v: usize) {
    p[i..i + 2].copy_from_slice(&(v as u16).to_le_bytes());
}
fn next_page(p: &Page) -> PageId {
    u32::from_le_bytes(p[0..4].try_into().unwrap())
}
fn n_slots(p: &Page) -> usize {
    u16_at(p, 4)
}
fn slot(p: &Page, i: usize) -> (usize, usize) {
    let base = HEADER + i * SLOT;
    (u16_at(p, base), u16_at(p, base + 2))
}

/// Formata uma página já alocada como página de heap vazia.
pub fn init_page(pager: &mut Pager, id: PageId) -> Result<()> {
    let mut page = [0u8; PAGE_SIZE];
    set_u16(&mut page, 6, PAGE_SIZE);
    pager.write(id, &page)
}

/// Cria uma nova tabela (heap) e devolve sua página raiz.
pub fn create(pager: &mut Pager) -> Result<PageId> {
    let id = pager.allocate()?;
    init_page(pager, id)?;
    Ok(id)
}

fn insert_in_page(p: &mut Page, data: &[u8]) -> Option<u16> {
    let n = n_slots(p);
    let free_end = u16_at(p, 6);
    let slots_end = HEADER + n * SLOT;
    let reuse = (0..n).find(|&i| slot(p, i).0 == 0);
    let need = data.len() + if reuse.is_some() { 0 } else { SLOT };
    if free_end < slots_end || free_end - slots_end < need {
        return None;
    }
    let off = free_end - data.len();
    p[off..off + data.len()].copy_from_slice(data);
    set_u16(p, 6, off);
    let i = match reuse {
        Some(i) => i,
        None => {
            set_u16(p, 4, n + 1);
            n
        }
    };
    set_u16(p, HEADER + i * SLOT, off);
    set_u16(p, HEADER + i * SLOT + 2, data.len());
    Some(i as u16)
}

pub fn insert(pager: &mut Pager, root: PageId, data: &[u8]) -> Result<RowId> {
    if data.is_empty() || data.len() > MAX_RECORD {
        return Err(Error::storage(format!(
            "registro de {} bytes não cabe numa página",
            data.len()
        )));
    }
    let mut id = root;
    loop {
        let mut page = pager.read(id)?;
        if let Some(slot) = insert_in_page(&mut page, data) {
            pager.write(id, &page)?;
            return Ok(RowId { page: id, slot });
        }
        let next = next_page(&page);
        if next != 0 {
            id = next;
        } else {
            let new = create(pager)?;
            page[0..4].copy_from_slice(&new.to_le_bytes());
            pager.write(id, &page)?;
            id = new;
        }
    }
}

pub fn scan(pager: &mut Pager, root: PageId) -> Result<Vec<(RowId, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut id = root;
    while id != 0 {
        let page = pager.read(id)?;
        for i in 0..n_slots(&page) {
            let (off, len) = slot(&page, i);
            if off != 0 {
                out.push((RowId { page: id, slot: i as u16 }, page[off..off + len].to_vec()));
            }
        }
        id = next_page(&page);
    }
    Ok(out)
}

/// Lê um registro pelo `RowId`; `None` se o slot foi apagado.
pub fn get(pager: &mut Pager, rid: RowId) -> Result<Option<Vec<u8>>> {
    let page = pager.read(rid.page)?;
    if (rid.slot as usize) >= n_slots(&page) {
        return Ok(None);
    }
    let (off, len) = slot(&page, rid.slot as usize);
    Ok((off != 0).then(|| page[off..off + len].to_vec()))
}

pub fn delete(pager: &mut Pager, rid: RowId) -> Result<()> {
    let mut page = pager.read(rid.page)?;
    if (rid.slot as usize) >= n_slots(&page) {
        return Err(Error::storage("slot inexistente"));
    }
    let base = HEADER + rid.slot as usize * SLOT;
    set_u16(&mut page, base, 0);
    set_u16(&mut page, base + 2, 0);
    pager.write(rid.page, &page)
}
