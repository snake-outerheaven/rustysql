//! B+tree em disco: índice `chave → RowId`. Cada nó ocupa uma página.
//!
//! * Folha:   `keys[i] → rids[i]`, encadeadas por `next`.
//! * Interno: `children.len() == keys.len() + 1`; a subárvore `children[i]` contém
//!   chaves `< keys[i]` e `children[i+1]` chaves `>= keys[i]`.
//!
//! A página raiz nunca muda de lugar (o catálogo guarda seu id): quando a raiz
//! divide, o conteúdo antigo é copiado para uma página nova e a raiz vira um nó
//! interno com dois filhos.
//!
//! Simplificação de estudo: `delete` só remove da folha, sem fundir nós.

use crate::error::{Error, Result};
use crate::heap::RowId;
use crate::pager::{PageId, Pager, PAGE_SIZE};
use crate::value::{decode_row, encode_row, Value};
use std::cmp::Ordering;

/// Limite do tamanho da chave codificada; garante ≥ 7 entradas por nó.
pub const MAX_KEY: usize = 512;

const LEAF: u8 = 1;
const INTERNAL: u8 = 2;

enum Node {
    Leaf { keys: Vec<Value>, rids: Vec<RowId>, next: PageId },
    Internal { keys: Vec<Value>, children: Vec<PageId> },
}

// ---- (de)serialização do nó ------------------------------------------

fn corrupt() -> Error {
    Error::storage("nó de B-tree corrompido")
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let s = self.buf.get(self.pos..self.pos + n).ok_or_else(corrupt)?;
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn key(&mut self) -> Result<Value> {
        let len = self.u16()? as usize;
        decode_row(self.take(len)?)?.into_iter().next().ok_or_else(corrupt)
    }
}

fn put_key(out: &mut Vec<u8>, k: &Value) {
    let kb = encode_row(std::slice::from_ref(k));
    out.extend_from_slice(&(kb.len() as u16).to_le_bytes());
    out.extend_from_slice(&kb);
}

impl Node {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Node::Leaf { keys, rids, next } => {
                out.push(LEAF);
                out.extend_from_slice(&(keys.len() as u16).to_le_bytes());
                out.extend_from_slice(&next.to_le_bytes());
                for (k, r) in keys.iter().zip(rids) {
                    put_key(&mut out, k);
                    out.extend_from_slice(&r.page.to_le_bytes());
                    out.extend_from_slice(&r.slot.to_le_bytes());
                }
            }
            Node::Internal { keys, children } => {
                out.push(INTERNAL);
                out.extend_from_slice(&(keys.len() as u16).to_le_bytes());
                out.extend_from_slice(&children[0].to_le_bytes());
                for (k, c) in keys.iter().zip(&children[1..]) {
                    put_key(&mut out, k);
                    out.extend_from_slice(&c.to_le_bytes());
                }
            }
        }
        out
    }

    fn decode(buf: &[u8]) -> Result<Node> {
        let mut r = Reader { buf, pos: 0 };
        let kind = r.u8()?;
        let n = r.u16()? as usize;
        let first = r.u32()?;
        match kind {
            LEAF => {
                let (mut keys, mut rids) = (Vec::new(), Vec::new());
                for _ in 0..n {
                    keys.push(r.key()?);
                    rids.push(RowId { page: r.u32()?, slot: r.u16()? });
                }
                Ok(Node::Leaf { keys, rids, next: first })
            }
            INTERNAL => {
                let (mut keys, mut children) = (Vec::new(), vec![first]);
                for _ in 0..n {
                    keys.push(r.key()?);
                    children.push(r.u32()?);
                }
                Ok(Node::Internal { keys, children })
            }
            _ => Err(corrupt()),
        }
    }

    fn fits(&self) -> bool {
        self.encode().len() <= PAGE_SIZE
    }
}

fn read_node(pager: &mut Pager, id: PageId) -> Result<Node> {
    Node::decode(&pager.read(id)?[..])
}

fn write_node(pager: &mut Pager, id: PageId, node: &Node) -> Result<()> {
    let bytes = node.encode();
    debug_assert!(bytes.len() <= PAGE_SIZE);
    let mut page = [0u8; PAGE_SIZE];
    page[..bytes.len()].copy_from_slice(&bytes);
    pager.write(id, &page)
}

// ---- API pública -----------------------------------------------------

/// Cria uma árvore vazia (uma folha) e devolve a página raiz.
pub fn create(pager: &mut Pager) -> Result<PageId> {
    let id = pager.allocate()?;
    write_node(pager, id, &Node::Leaf { keys: vec![], rids: vec![], next: 0 })?;
    Ok(id)
}

pub fn search(pager: &mut Pager, root: PageId, key: &Value) -> Result<Option<RowId>> {
    let mut id = root;
    loop {
        match read_node(pager, id)? {
            Node::Internal { keys, children } => {
                id = children[keys.partition_point(|k| k.total_cmp(key) != Ordering::Greater)];
            }
            Node::Leaf { keys, rids, .. } => {
                return Ok(keys.binary_search_by(|k| k.total_cmp(key)).ok().map(|i| rids[i]));
            }
        }
    }
}

pub fn insert(pager: &mut Pager, root: PageId, key: &Value, rid: RowId) -> Result<()> {
    if *key == Value::Null {
        return Err(Error::exec("chave de índice não pode ser NULL"));
    }
    if encode_row(std::slice::from_ref(key)).len() > MAX_KEY {
        return Err(Error::exec(format!("chave maior que {MAX_KEY} bytes")));
    }
    if let Some((sep, right)) = insert_rec(pager, root, key, rid)? {
        // a raiz dividiu: move a metade esquerda para uma página nova
        let left = read_node(pager, root)?;
        let left_id = pager.allocate()?;
        write_node(pager, left_id, &left)?;
        write_node(pager, root, &Node::Internal { keys: vec![sep], children: vec![left_id, right] })?;
    }
    Ok(())
}

/// Remove a chave; devolve `true` se existia.
pub fn delete(pager: &mut Pager, root: PageId, key: &Value) -> Result<bool> {
    let mut id = root;
    loop {
        match read_node(pager, id)? {
            Node::Internal { keys, children } => {
                id = children[keys.partition_point(|k| k.total_cmp(key) != Ordering::Greater)];
            }
            Node::Leaf { mut keys, mut rids, next } => {
                let Ok(i) = keys.binary_search_by(|k| k.total_cmp(key)) else {
                    return Ok(false);
                };
                keys.remove(i);
                rids.remove(i);
                write_node(pager, id, &Node::Leaf { keys, rids, next })?;
                return Ok(true);
            }
        }
    }
}

/// Nº de chaves e altura da árvore (para inspeção/estudo).
pub fn stats(pager: &mut Pager, root: PageId) -> Result<(usize, usize)> {
    let mut height = 1;
    let mut id = root;
    while let Node::Internal { children, .. } = read_node(pager, id)? {
        id = children[0];
        height += 1;
    }
    let mut count = 0;
    loop {
        let Node::Leaf { keys, next, .. } = read_node(pager, id)? else {
            return Err(Error::storage("cadeia de folhas inválida"));
        };
        count += keys.len();
        if next == 0 {
            return Ok((count, height));
        }
        id = next;
    }
}

// ---- inserção recursiva com split -----------------------------------

fn insert_rec(
    pager: &mut Pager,
    id: PageId,
    key: &Value,
    rid: RowId,
) -> Result<Option<(Value, PageId)>> {
    let node = match read_node(pager, id)? {
        Node::Leaf { mut keys, mut rids, next } => {
            let pos = match keys.binary_search_by(|k| k.total_cmp(key)) {
                Ok(_) => return Err(Error::exec(format!("chave duplicada no índice: {key}"))),
                Err(p) => p,
            };
            keys.insert(pos, key.clone());
            rids.insert(pos, rid);
            Node::Leaf { keys, rids, next }
        }
        Node::Internal { mut keys, mut children } => {
            let idx = keys.partition_point(|k| k.total_cmp(key) != Ordering::Greater);
            match insert_rec(pager, children[idx], key, rid)? {
                None => return Ok(None),
                Some((sep, right)) => {
                    keys.insert(idx, sep);
                    children.insert(idx + 1, right);
                    Node::Internal { keys, children }
                }
            }
        }
    };
    if node.fits() {
        write_node(pager, id, &node)?;
        return Ok(None);
    }
    split(pager, id, node).map(Some)
}

/// Divide um nó cheio ao meio; devolve (chave separadora, página nova à direita).
fn split(pager: &mut Pager, id: PageId, node: Node) -> Result<(Value, PageId)> {
    let new_id = pager.allocate()?;
    match node {
        Node::Leaf { mut keys, mut rids, next } => {
            let mid = keys.len() / 2;
            let (rk, rr) = (keys.split_off(mid), rids.split_off(mid));
            let sep = rk[0].clone();
            write_node(pager, new_id, &Node::Leaf { keys: rk, rids: rr, next })?;
            write_node(pager, id, &Node::Leaf { keys, rids, next: new_id })?;
            Ok((sep, new_id))
        }
        Node::Internal { mut keys, mut children } => {
            let mid = keys.len() / 2;
            let mut rk = keys.split_off(mid);
            let sep = rk.remove(0); // a chave do meio sobe
            let rc = children.split_off(mid + 1);
            write_node(pager, new_id, &Node::Internal { keys: rk, children: rc })?;
            write_node(pager, id, &Node::Internal { keys, children })?;
            Ok((sep, new_id))
        }
    }
}
