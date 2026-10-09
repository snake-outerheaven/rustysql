//! Buffer pool: cache de páginas com latches.
//!
//! * Cada página em memória é um `Arc<Frame>`. O `Arc` é o **pin**: enquanto alguém
//!   guarda um `PageRef` (ou um guard), a página não pode ser despejada.
//! * O conteúdo fica atrás de um `RwLock` (o **latch**). Os guards são *owned*
//!   (carregam o `Arc`), o que permite segurar o latch do pai enquanto se pega o do
//!   filho e depois soltar o pai: o *latch crabbing* da B-tree.
//! * Política **no-steal**: páginas sujas nunca são despejadas nem gravadas fora de
//!   um checkpoint, então o arquivo de dados só contém estados commitados
//!   (veja `wal.rs` e `Inner::checkpoint`).
//! * Despejo por relógio (*second chance*).

use crate::disk::{DiskManager, PageBuf, PageId, PAGE_SIZE};
use crate::error::{Error, Result};
use parking_lot::{ArcRwLockReadGuard, ArcRwLockWriteGuard, Mutex, RawRwLock, RwLock};
use std::collections::{HashMap, VecDeque};
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

/// Uma página em memória: o conteúdo (atrás de um latch), o bit de sujeira e o bit de
/// referência do relógio.
///
/// É sempre usada como [`PageRef`] (`Arc<Frame>`); o contador do `Arc` é o **pin**: enquanto
/// alguém guarda uma referência, a página não pode ser despejada.
pub struct Frame {
    id: PageId,
    latch: Arc<RwLock<Box<PageBuf>>>,
    dirty: AtomicBool,
    referenced: AtomicBool,
    dirty_count: Arc<AtomicUsize>,
}

/// Referência fixada (*pinned*) a uma página do pool. Clonar incrementa o pin; soltar o último
/// clone libera a página para despejo.
pub type PageRef = Arc<Frame>;

impl Frame {
    fn new(id: PageId, buf: Box<PageBuf>, dirty: bool, dirty_count: Arc<AtomicUsize>) -> Frame {
        if dirty {
            dirty_count.fetch_add(1, Ordering::SeqCst);
        }
        Frame {
            id,
            latch: Arc::new(RwLock::new(buf)),
            dirty: AtomicBool::new(dirty),
            referenced: AtomicBool::new(true),
            dirty_count,
        }
    }

    /// Identificador da página no arquivo.
    pub fn id(&self) -> PageId {
        self.id
    }

    fn mark_dirty(&self) {
        if !self.dirty.swap(true, Ordering::SeqCst) {
            self.dirty_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Latch compartilhado.
    pub fn read(self: &Arc<Self>) -> PageReadGuard {
        PageReadGuard { guard: self.latch.read_arc(), pin: self.clone() }
    }

    /// Latch exclusivo.
    pub fn write(self: &Arc<Self>) -> PageWriteGuard {
        PageWriteGuard { guard: self.latch.write_arc(), pin: self.clone() }
    }
}

/// Latch compartilhado de uma página. Várias threads podem ter um ao mesmo tempo. Carrega o
/// [`PageRef`], então a página fica fixada enquanto o guard existir. Dá acesso (`Deref`) ao
/// conteúdo de 4096 bytes.
pub struct PageReadGuard {
    guard: ArcRwLockReadGuard<RawRwLock, Box<PageBuf>>,
    pin: PageRef,
}

impl PageReadGuard {
    /// Identificador da página protegida.
    pub fn id(&self) -> PageId {
        self.pin.id
    }
}

impl Deref for PageReadGuard {
    type Target = PageBuf;
    fn deref(&self) -> &PageBuf {
        &self.guard
    }
}

/// Latch exclusivo de uma página. Só uma thread por vez, e nenhum leitor junto.
///
/// A página só é marcada como suja quando o conteúdo é de fato modificado (`DerefMut`),
/// então pegar o latch para olhar não suja a página.
pub struct PageWriteGuard {
    guard: ArcRwLockWriteGuard<RawRwLock, Box<PageBuf>>,
    pin: PageRef,
}

impl PageWriteGuard {
    /// Identificador da página protegida.
    pub fn id(&self) -> PageId {
        self.pin.id
    }
}

impl Deref for PageWriteGuard {
    type Target = PageBuf;
    fn deref(&self) -> &PageBuf {
        &self.guard
    }
}

impl DerefMut for PageWriteGuard {
    /// Só marca a página como suja quando alguém realmente a modifica.
    fn deref_mut(&mut self) -> &mut PageBuf {
        self.pin.mark_dirty();
        &mut self.guard
    }
}

struct State {
    frames: HashMap<PageId, PageRef>,
    clock: VecDeque<PageId>,
    next_page: u32,
}

/// Cache de páginas com latches.
///
/// # Política
/// * **No-steal**: páginas sujas nunca são despejadas nem gravadas fora de [`BufferPool::flush_all`]
///   (o checkpoint). O arquivo de dados só contém estados commitados.
/// * **Despejo por relógio** (*second chance*), pulando páginas fixadas e sujas.
///
/// # Concorrência
/// `BufferPool` é `Send + Sync`. A tabela de páginas fica atrás de um mutex curto (nunca
/// segurado enquanto se espera um latch de página); cada página tem o seu `RwLock`.
///
/// # Limite
/// Se o pool está cheio de páginas sujas ou fixadas, [`BufferPool::fetch`] e
/// [`BufferPool::new_page`] falham: é o limite de tamanho de uma transação.
pub struct BufferPool {
    disk: Mutex<DiskManager>,
    state: Mutex<State>,
    capacity: usize,
    dirty_count: Arc<AtomicUsize>,
}

impl BufferPool {
    /// Abre o arquivo e cria um pool de `capacity` páginas (mínimo de 16).
    ///
    /// # Errors
    /// Falha se o arquivo não pode ser aberto ou não tem o cabeçalho esperado.
    pub fn open(path: &Path, capacity: usize) -> Result<BufferPool> {
        let (disk, pages) = DiskManager::open(path)?;
        Ok(BufferPool {
            disk: Mutex::new(disk),
            state: Mutex::new(State {
                frames: HashMap::new(),
                clock: VecDeque::new(),
                next_page: pages,
            }),
            capacity: capacity.max(16),
            dirty_count: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Número máximo de páginas em memória.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Quantas páginas estão sujas (aguardando o próximo checkpoint).
    pub fn dirty_pages(&self) -> usize {
        self.dirty_count.load(Ordering::SeqCst)
    }

    /// Número de páginas do arquivo, contando as ainda não gravadas e a página 0 (cabeçalho).
    pub fn num_pages(&self) -> u32 {
        self.state.lock().next_page
    }

    /// Procura uma vítima com o relógio. Páginas fixadas (pin) ou sujas são puladas.
    fn make_room(&self, st: &mut State) -> Result<()> {
        if st.frames.len() < self.capacity {
            return Ok(());
        }
        for _ in 0..st.clock.len() * 2 + 1 {
            let Some(id) = st.clock.pop_front() else { break };
            let frame = &st.frames[&id];
            let evictable =
                Arc::strong_count(frame) == 1 && !frame.dirty.load(Ordering::SeqCst);
            if evictable && !frame.referenced.swap(false, Ordering::SeqCst) {
                st.frames.remove(&id);
                return Ok(());
            }
            st.clock.push_back(id);
        }
        Err(Error::storage(
            "buffer pool cheio de páginas sujas/fixadas: transação grande demais para o pool",
        ))
    }

    fn load(&self, st: &mut State, id: PageId) -> Result<PageRef> {
        self.make_room(st)?;
        let mut buf = Box::new([0u8; PAGE_SIZE]);
        self.disk.lock().read_page(id, &mut buf)?;
        let frame = Arc::new(Frame::new(id, buf, false, self.dirty_count.clone()));
        st.frames.insert(id, frame.clone());
        st.clock.push_back(id);
        Ok(frame)
    }

    /// Devolve a página `id`, lendo-a do disco se não estiver no pool.
    ///
    /// # Errors
    /// Falha se `id` não existe, se o disco falha ou se o pool está cheio de páginas que não
    /// podem ser despejadas.
    pub fn fetch(&self, id: PageId) -> Result<PageRef> {
        let mut st = self.state.lock();
        if let Some(f) = st.frames.get(&id) {
            f.referenced.store(true, Ordering::Relaxed);
            return Ok(f.clone());
        }
        if id == 0 || id >= st.next_page {
            return Err(Error::storage(format!("página {id} inexistente")));
        }
        self.load(&mut st, id)
    }

    /// Aloca uma página nova (zerada e suja, para só ir ao disco no checkpoint).
    pub fn new_page(&self) -> Result<PageRef> {
        let mut st = self.state.lock();
        self.make_room(&mut st)?;
        let id = st.next_page;
        st.next_page += 1;
        let frame =
            Arc::new(Frame::new(id, Box::new([0u8; PAGE_SIZE]), true, self.dirty_count.clone()));
        st.frames.insert(id, frame.clone());
        st.clock.push_back(id);
        Ok(frame)
    }

    /// Para a recuperação: garante que a página `id` exista, criando-a zerada se
    /// ela está além do fim do arquivo.
    pub fn ensure_page(&self, id: PageId) -> Result<PageRef> {
        let mut st = self.state.lock();
        if let Some(f) = st.frames.get(&id) {
            return Ok(f.clone());
        }
        if id < st.next_page {
            return self.load(&mut st, id);
        }
        self.make_room(&mut st)?;
        st.next_page = id + 1;
        let frame =
            Arc::new(Frame::new(id, Box::new([0u8; PAGE_SIZE]), true, self.dirty_count.clone()));
        st.frames.insert(id, frame.clone());
        st.clock.push_back(id);
        Ok(frame)
    }

    /// Grava todas as páginas sujas e força o disco (usado pelo checkpoint, quando
    /// não há transações ativas).
    pub fn flush_all(&self) -> Result<()> {
        let dirty: Vec<PageRef> = {
            let st = self.state.lock();
            st.frames.values().filter(|f| f.dirty.load(Ordering::SeqCst)).cloned().collect()
        };
        let mut disk = self.disk.lock();
        for f in dirty {
            if f.dirty.swap(false, Ordering::SeqCst) {
                self.dirty_count.fetch_sub(1, Ordering::SeqCst);
                let g = f.read();
                disk.write_page(f.id, &g)?;
            }
        }
        disk.sync()
    }
}
