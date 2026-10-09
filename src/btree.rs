//! B+tree em disco, thread-safe, com **latch crabbing**.
//!
//! Cada nó é uma página do buffer pool, protegida por um `RwLock` (latch). A árvore é
//! hierárquica, então a regra que garante a integridade é sempre travar **de cima para
//! baixo** e só soltar o latch do pai depois de ter o do filho:
//!
//! * **busca**: latch compartilhado no filho, depois solta o pai;
//! * **inserção otimista**: desce com latches compartilhados, pega latch exclusivo só
//!   na folha; se ela comporta a chave, insere (caso comum);
//! * **inserção pessimista** (a folha vai dividir): desce com latches exclusivos e, a
//!   cada filho *seguro* (que absorve uma inserção sem dividir), solta todos os
//!   ancestrais, pois a divisão nunca vai subir além dele. Os ancestrais que sobram
//!   no `path` são exatamente os que podem ser modificados por um split;
//! * **varredura de intervalo**: segura um latch por vez; segue o ponteiro `next` das
//!   folhas (lido sob latch, então nunca perde nem repete uma chave que já existia).
//!
//! Como todo mundo trava na mesma ordem (raiz → folha, esquerda → direita), não há
//! deadlock de latch. A página raiz nunca muda de lugar (o catálogo guarda o id): ao
//! dividir, o conteúdo é copiado para uma página nova.
//!
//! Chaves são bytes (`keyenc`); valores são `RowId`. `delete` só remove da folha, sem
//! fundir nós (simplificação de estudo).

use crate::bufferpool::{BufferPool, PageWriteGuard};
use crate::disk::{PageBuf, PageId, PAGE_SIZE};
use crate::error::{Error, Result};
use crate::heap::RowId;
use std::collections::VecDeque;
use std::sync::Arc;

/// Tamanho máximo, em bytes, de uma chave de índice já codificada (veja [`keyenc`](crate::keyenc)).
///
/// O limite garante que um nó de 4 KB sempre comporta várias entradas, o que permite dividir
/// qualquer nó cheio em duas metades que cabem numa página.
pub const MAX_KEY: usize = 512;
const HDR: usize = 8;
/// maior acréscimo que uma inserção pode causar num nó
const MAX_ENTRY: usize = 2 + MAX_KEY + 6;

const LEAF: u8 = 1;
const INTERNAL: u8 = 2;

#[derive(Debug, Clone)]
enum Node {
    Leaf { keys: Vec<Vec<u8>>, rids: Vec<RowId>, next: PageId },
    /// `children.len() == keys.len() + 1`; `children[i]` tem chaves `< keys[i]`
    Internal { level: u8, keys: Vec<Vec<u8>>, children: Vec<PageId> },
}

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
    fn key(&mut self) -> Result<Vec<u8>> {
        let n = self.u16()? as usize;
        Ok(self.take(n)?.to_vec())
    }
}

impl Node {
    fn empty_leaf() -> Node {
        Node::Leaf { keys: vec![], rids: vec![], next: 0 }
    }

    fn level(&self) -> u8 {
        match self {
            Node::Leaf { .. } => 0,
            Node::Internal { level, .. } => *level,
        }
    }

    fn keys(&self) -> &[Vec<u8>] {
        match self {
            Node::Leaf { keys, .. } | Node::Internal { keys, .. } => keys,
        }
    }

    fn encoded_len(&self) -> usize {
        match self {
            Node::Leaf { keys, .. } => HDR + keys.iter().map(|k| 2 + k.len() + 6).sum::<usize>(),
            Node::Internal { keys, .. } => HDR + keys.iter().map(|k| 2 + k.len() + 4).sum::<usize>(),
        }
    }

    fn is_safe_for_insert(&self) -> bool {
        self.encoded_len() + MAX_ENTRY <= PAGE_SIZE
    }

    fn decode(page: &PageBuf) -> Result<Node> {
        let mut r = Reader { buf: &page[..], pos: 0 };
        let kind = r.u8()?;
        let level = r.u8()?;
        let n = r.u16()? as usize;
        let first = r.u32()?;
        match kind {
            LEAF => {
                let (mut keys, mut rids) = (Vec::with_capacity(n), Vec::with_capacity(n));
                for _ in 0..n {
                    keys.push(r.key()?);
                    rids.push(RowId { page: r.u32()?, slot: r.u16()? });
                }
                Ok(Node::Leaf { keys, rids, next: first })
            }
            INTERNAL => {
                let (mut keys, mut children) = (Vec::with_capacity(n), vec![first]);
                for _ in 0..n {
                    keys.push(r.key()?);
                    children.push(r.u32()?);
                }
                Ok(Node::Internal { level, keys, children })
            }
            _ => Err(corrupt()),
        }
    }

    fn encode_into(&self, page: &mut PageBuf) {
        let mut out = Vec::with_capacity(self.encoded_len());
        match self {
            Node::Leaf { keys, rids, next } => {
                out.extend_from_slice(&[LEAF, 0]);
                out.extend_from_slice(&(keys.len() as u16).to_le_bytes());
                out.extend_from_slice(&next.to_le_bytes());
                for (k, r) in keys.iter().zip(rids) {
                    out.extend_from_slice(&(k.len() as u16).to_le_bytes());
                    out.extend_from_slice(k);
                    out.extend_from_slice(&r.page.to_le_bytes());
                    out.extend_from_slice(&r.slot.to_le_bytes());
                }
            }
            Node::Internal { level, keys, children } => {
                out.extend_from_slice(&[INTERNAL, *level]);
                out.extend_from_slice(&(keys.len() as u16).to_le_bytes());
                out.extend_from_slice(&children[0].to_le_bytes());
                for (k, c) in keys.iter().zip(&children[1..]) {
                    out.extend_from_slice(&(k.len() as u16).to_le_bytes());
                    out.extend_from_slice(k);
                    out.extend_from_slice(&c.to_le_bytes());
                }
            }
        }
        assert!(out.len() <= PAGE_SIZE, "nó não cabe na página");
        page[..out.len()].copy_from_slice(&out);
    }

    /// Divide ao meio (por bytes). `self` fica com a metade esquerda; devolve a chave
    /// separadora e a metade direita. `right_id` é a página que receberá a direita.
    fn split(&mut self, right_id: PageId) -> (Vec<u8>, Node) {
        let sizes: Vec<usize> = match self {
            Node::Leaf { keys, .. } => keys.iter().map(|k| 2 + k.len() + 6).collect(),
            Node::Internal { keys, .. } => keys.iter().map(|k| 2 + k.len() + 4).collect(),
        };
        let total: usize = sizes.iter().sum();
        let (mut acc, mut mid) = (0, 0);
        while mid < sizes.len() && acc + sizes[mid] <= total / 2 {
            acc += sizes[mid];
            mid += 1;
        }
        let mid = mid.clamp(1, sizes.len() - 1);
        match self {
            Node::Leaf { keys, rids, next } => {
                let rk = keys.split_off(mid);
                let rr = rids.split_off(mid);
                let sep = rk[0].clone();
                let right = Node::Leaf { keys: rk, rids: rr, next: *next };
                *next = right_id;
                (sep, right)
            }
            Node::Internal { level, keys, children } => {
                let mut rk = keys.split_off(mid);
                let sep = rk.remove(0); // a chave do meio sobe para o pai
                let rc = children.split_off(mid + 1);
                (sep, Node::Internal { level: *level, keys: rk, children: rc })
            }
        }
    }
}

/// Número de chaves `<= key`: índice do filho por onde descer.
fn upper_bound(keys: &[Vec<u8>], key: &[u8]) -> usize {
    keys.partition_point(|k| k.as_slice() <= key)
}

/// Estatísticas estruturais de uma árvore, calculadas por [`BTree::verify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeStats {
    /// Total de chaves nas folhas.
    pub keys: usize,
    /// Número de níveis (1 = a raiz é uma folha).
    pub height: usize,
    /// Número de folhas.
    pub leaves: usize,
    /// Número total de nós (folhas e internos).
    pub nodes: usize,
}

/// Handle barato (`Clone`) para uma árvore; várias threads podem usar cópias dele.
#[derive(Clone)]
pub struct BTree {
    pool: Arc<BufferPool>,
    root: PageId,
}

impl BTree {
    /// Cria uma árvore vazia (uma raiz que é folha) numa página nova do pool.
    ///
    /// # Errors
    /// Falha se o pool não tem espaço para a página (veja [`BufferPool::new_page`]).
    pub fn create(pool: &Arc<BufferPool>) -> Result<BTree> {
        let frame = pool.new_page()?;
        Node::empty_leaf().encode_into(&mut frame.write());
        Ok(BTree { pool: pool.clone(), root: frame.id() })
    }

    /// Abre uma árvore existente a partir da página raiz guardada no catálogo.
    pub fn open(pool: Arc<BufferPool>, root: PageId) -> BTree {
        BTree { pool, root }
    }

    /// Página raiz. Nunca muda durante a vida da árvore: quando a raiz divide, o conteúdo é
    /// copiado para uma página nova e a raiz vira um nó interno.
    pub fn root(&self) -> PageId {
        self.root
    }

    /// Zera a árvore (a raiz vira uma folha vazia). Usado ao reconstruir índices.
    pub fn reset(&self) -> Result<()> {
        let frame = self.pool.ensure_page(self.root)?;
        Node::empty_leaf().encode_into(&mut frame.write());
        Ok(())
    }

    fn check_key(key: &[u8]) -> Result<()> {
        if key.is_empty() || key.len() > MAX_KEY {
            return Err(Error::exec(format!("chave de índice com {} bytes (máximo {MAX_KEY})", key.len())));
        }
        Ok(())
    }

    // ---- leitura -------------------------------------------------------

    /// Desce com latches compartilhados encadeados até a folha que cobre `key`.
    fn find_leaf_read(&self, key: &[u8]) -> Result<Node> {
        let mut guard = self.pool.fetch(self.root)?.read();
        loop {
            match Node::decode(&guard)? {
                leaf @ Node::Leaf { .. } => return Ok(leaf),
                Node::Internal { keys, children, .. } => {
                    let child = self.pool.fetch(children[upper_bound(&keys, key)])?;
                    guard = child.read(); // pega o filho, só então o pai é solto
                }
            }
        }
    }

    /// Procura uma chave exata.
    ///
    /// # Concorrência
    /// Desce com **latches compartilhados encadeados**: pega o latch do filho antes de soltar o
    /// do pai, então nunca enxerga um nó no meio de uma divisão. Não bloqueia outros leitores.
    ///
    /// # Errors
    /// Falha se uma página não pode ser lida ou está corrompida.
    pub fn search(&self, key: &[u8]) -> Result<Option<RowId>> {
        let Node::Leaf { keys, rids, .. } = self.find_leaf_read(key)? else { unreachable!() };
        Ok(keys.binary_search_by(|k| k.as_slice().cmp(key)).ok().map(|i| rids[i]))
    }

    /// Entradas com `start <= chave < end` (`end = None`: até o fim), em ordem.
    pub fn scan(&self, start: &[u8], end: Option<&[u8]>) -> Result<BTreeScan> {
        let Node::Leaf { keys, rids, next } = self.find_leaf_read(start)? else { unreachable!() };
        let from = keys.partition_point(|k| k.as_slice() < start);
        Ok(BTreeScan {
            pool: self.pool.clone(),
            buf: keys.into_iter().zip(rids).skip(from).collect(),
            next,
            end: end.map(<[u8]>::to_vec),
            done: false,
        })
    }

    // ---- escrita -------------------------------------------------------

    /// Latch exclusivo só na folha; os nós internos usam latches compartilhados.
    fn find_leaf_write(&self, key: &[u8]) -> Result<(PageWriteGuard, Node)> {
        loop {
            let root = self.pool.fetch(self.root)?;
            let rg = root.read();
            let rn = Node::decode(&rg)?;
            if rn.level() == 0 {
                // raiz é folha: troca o latch compartilhado pelo exclusivo e revalida
                drop(rg);
                let wg = root.write();
                let n = Node::decode(&wg)?;
                if n.level() == 0 {
                    return Ok((wg, n));
                }
                continue; // a raiz dividiu enquanto não segurávamos o latch
            }
            let (mut cur, mut curnode) = (rg, rn);
            loop {
                let Node::Internal { level, keys, children } = &curnode else { unreachable!() };
                let (level, child_id) = (*level, children[upper_bound(keys, key)]);
                let child = self.pool.fetch(child_id)?;
                if level == 1 {
                    let wg = child.write(); // filho folha: exclusivo...
                    drop(cur); // ...e só então o pai é solto
                    let leaf = Node::decode(&wg)?;
                    return Ok((wg, leaf));
                }
                let cg = child.read();
                curnode = Node::decode(&cg)?;
                cur = cg;
            }
        }
    }

    /// `Ok(false)` se a chave já existe.
    pub fn insert(&self, key: &[u8], rid: RowId) -> Result<bool> {
        Self::check_key(key)?;
        let (mut guard, mut leaf) = self.find_leaf_write(key)?;
        let Node::Leaf { keys, rids, .. } = &mut leaf else { unreachable!() };
        let pos = match keys.binary_search_by(|k| k.as_slice().cmp(key)) {
            Ok(_) => return Ok(false),
            Err(p) => p,
        };
        if 2 + key.len() + 6 + HDR + keys.iter().map(|k| 2 + k.len() + 6).sum::<usize>() <= PAGE_SIZE {
            keys.insert(pos, key.to_vec());
            rids.insert(pos, rid);
            leaf.encode_into(&mut guard);
            return Ok(true);
        }
        drop(guard); // vai dividir: recomeça com a descida pessimista
        self.insert_pessimistic(key, rid)
    }

    fn insert_pessimistic(&self, key: &[u8], rid: RowId) -> Result<bool> {
        let mut path: Vec<(PageWriteGuard, Node)> = Vec::new();
        let root = self.pool.fetch(self.root)?.write();
        let node = Node::decode(&root)?;
        path.push((root, node));
        loop {
            let child_id = match &path.last().unwrap().1 {
                Node::Leaf { .. } => break,
                Node::Internal { keys, children, .. } => children[upper_bound(keys, key)],
            };
            let guard = self.pool.fetch(child_id)?.write();
            let node = Node::decode(&guard)?;
            if node.is_safe_for_insert() {
                path.clear(); // nenhum split chega aos ancestrais: solta todos
            }
            path.push((guard, node));
        }

        {
            let (_, leaf) = path.last_mut().unwrap();
            let Node::Leaf { keys, rids, .. } = leaf else { unreachable!() };
            match keys.binary_search_by(|k| k.as_slice().cmp(key)) {
                Ok(_) => return Ok(false),
                Err(p) => {
                    keys.insert(p, key.to_vec());
                    rids.insert(p, rid);
                }
            }
        }

        let mut pending: Option<(Vec<u8>, PageId)> = None;
        loop {
            let (mut guard, mut node) = path.pop().expect("o caminho sempre contém o nó a dividir");
            if let Some((sep, right)) = pending.take() {
                let Node::Internal { keys, children, .. } = &mut node else { unreachable!() };
                let idx = upper_bound(keys, &sep);
                keys.insert(idx, sep);
                children.insert(idx + 1, right);
            }
            if node.encoded_len() <= PAGE_SIZE {
                node.encode_into(&mut guard);
                return Ok(true);
            }

            let right_page = self.pool.new_page()?;
            let (sep, right) = node.split(right_page.id());
            right.encode_into(&mut right_page.write());

            if path.is_empty() {
                // `guard` é a raiz: a metade esquerda vai para uma página nova e a raiz
                // (que não pode mudar de id) passa a ser um nó interno com 2 filhos
                let left_page = self.pool.new_page()?;
                node.encode_into(&mut left_page.write());
                let level = node.level() + 1;
                Node::Internal { level, keys: vec![sep], children: vec![left_page.id(), right_page.id()] }
                    .encode_into(&mut guard);
                return Ok(true);
            }
            node.encode_into(&mut guard);
            pending = Some((sep, right_page.id()));
        }
    }

    /// `Ok(true)` se a chave existia.
    pub fn delete(&self, key: &[u8]) -> Result<bool> {
        let (mut guard, mut leaf) = self.find_leaf_write(key)?;
        let Node::Leaf { keys, rids, .. } = &mut leaf else { unreachable!() };
        let Ok(i) = keys.binary_search_by(|k| k.as_slice().cmp(key)) else { return Ok(false) };
        keys.remove(i);
        rids.remove(i);
        leaf.encode_into(&mut guard);
        Ok(true)
    }

    // ---- inspeção ------------------------------------------------------

    /// Confere todos os invariantes da árvore (use com a árvore quiescente):
    /// chaves ordenadas, separadores respeitados, níveis coerentes e cadeia de folhas
    /// igual à ordem da travessia.
    pub fn verify(&self) -> Result<TreeStats> {
        let mut leaves = Vec::new();
        let mut stats = TreeStats { keys: 0, height: 0, leaves: 0, nodes: 0 };
        self.verify_node(self.root, None, None, None, &mut leaves, &mut stats)?;

        let mut chain = Vec::new();
        let mut id = leaves[0];
        loop {
            chain.push(id);
            let Node::Leaf { next, .. } = Node::decode(&self.pool.fetch(id)?.read())? else {
                return Err(Error::storage("cadeia de folhas aponta para nó interno"));
            };
            if next == 0 {
                break;
            }
            id = next;
        }
        if chain != leaves {
            return Err(Error::storage("cadeia de folhas diferente da ordem da árvore"));
        }
        stats.leaves = leaves.len();
        Ok(stats)
    }

    fn verify_node(
        &self,
        id: PageId,
        lo: Option<&[u8]>,
        hi: Option<&[u8]>,
        expect_level: Option<u8>,
        leaves: &mut Vec<PageId>,
        stats: &mut TreeStats,
    ) -> Result<()> {
        let bad = |m: &str| Error::storage(format!("B-tree inválida (página {id}): {m}"));
        let node = Node::decode(&self.pool.fetch(id)?.read())?;
        stats.nodes += 1;
        if expect_level.is_some_and(|l| node.level() != l) {
            return Err(bad("nível inesperado"));
        }
        stats.height = stats.height.max(node.level() as usize + 1);
        let keys = node.keys();
        if keys.windows(2).any(|w| w[0] >= w[1]) {
            return Err(bad("chaves fora de ordem"));
        }
        if lo.is_some_and(|l| keys.first().is_some_and(|k| k.as_slice() < l))
            || hi.is_some_and(|h| keys.last().is_some_and(|k| k.as_slice() >= h))
        {
            return Err(bad("chave fora dos limites do pai"));
        }
        match &node {
            Node::Leaf { keys, .. } => {
                stats.keys += keys.len();
                leaves.push(id);
            }
            Node::Internal { level, keys, children } => {
                if children.len() != keys.len() + 1 {
                    return Err(bad("nº de filhos incoerente"));
                }
                for (i, &c) in children.iter().enumerate() {
                    let clo = if i == 0 { lo } else { Some(keys[i - 1].as_slice()) };
                    let chi = if i == keys.len() { hi } else { Some(keys[i].as_slice()) };
                    self.verify_node(c, clo, chi, Some(level - 1), leaves, stats)?;
                }
            }
        }
        Ok(())
    }

    /// Estatísticas da árvore. É um sinônimo de [`BTree::verify`], que também confere os
    /// invariantes.
    pub fn stats(&self) -> Result<TreeStats> {
        self.verify()
    }
}

/// Iterador de intervalo: um latch por vez, folha a folha.
pub struct BTreeScan {
    pool: Arc<BufferPool>,
    buf: VecDeque<(Vec<u8>, RowId)>,
    next: PageId,
    end: Option<Vec<u8>>,
    done: bool,
}

impl Iterator for BTreeScan {
    type Item = Result<(Vec<u8>, RowId)>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.done {
                return None;
            }
            if let Some((k, r)) = self.buf.pop_front() {
                if self.end.as_ref().is_some_and(|e| k.as_slice() >= e.as_slice()) {
                    self.done = true;
                    return None;
                }
                return Some(Ok((k, r)));
            }
            if self.next == 0 {
                self.done = true;
                return None;
            }
            let leaf = self
                .pool
                .fetch(self.next)
                .and_then(|f| Node::decode(&f.read()));
            match leaf {
                Ok(Node::Leaf { keys, rids, next }) => {
                    self.buf = keys.into_iter().zip(rids).collect();
                    self.next = next;
                }
                Ok(_) => {
                    self.done = true;
                    return Some(Err(corrupt()));
                }
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            }
        }
    }
}
