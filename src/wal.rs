//! WAL (write-ahead log) de **redo**, um registro por transação commitada.
//!
//! Política: *no-steal / no-force*.
//! * **no-steal**: páginas sujas só vão ao disco num checkpoint, então o arquivo de
//!   dados nunca contém mudanças de transações não commitadas (não precisa de undo
//!   em disco; o rollback é feito em memória).
//! * **no-force**: o commit só grava e força o WAL, não as páginas de dados.
//!
//! O commit é **atômico**: todas as operações da transação viram um único registro
//! `[len u32][crc32 u32][payload]`. Um registro cortado ou corrompido no fim do
//! arquivo (queda no meio da escrita) é descartado na abertura: a transação não
//! estava commitada.
//!
//! Só operações do heap são logadas. Os índices (B-trees) são derivados dos dados
//! e reconstruídos após uma recuperação.

use crate::disk::PageId;
use crate::error::Result;
use parking_lot::Mutex;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Uma operação de redo sobre o heap. Só o heap é logado: os índices são reconstruídos na
/// recuperação.
#[derive(Debug, Clone, PartialEq)]
pub enum WalOp {
    /// página nova formatada como página de heap vazia
    HeapFormat {
        /// Página recém-alocada.
        page: PageId,
    },
    /// `prev.next = next` na cadeia de páginas de uma tabela
    HeapLink {
        /// Última página da cadeia até então.
        prev: PageId,
        /// Página nova, que passa a ser a seguinte.
        next: PageId,
    },
    /// Coloca `data` exatamente no slot `slot` da página (idempotente).
    HeapInsert {
        /// Página do registro.
        page: PageId,
        /// Slot do registro.
        slot: u16,
        /// Conteúdo serializado da linha.
        data: Vec<u8>,
    },
    /// Libera o slot (idempotente).
    HeapDelete {
        /// Página do registro.
        page: PageId,
        /// Slot do registro.
        slot: u16,
    },
}

impl WalOp {
    /// Operações estruturais sobrevivem ao rollback (a página continua alocada e
    /// encadeada, e transações futuras vão escrever nela).
    pub fn is_structural(&self) -> bool {
        matches!(self, WalOp::HeapFormat { .. } | WalOp::HeapLink { .. })
    }
}

/// Um registro do WAL: tudo o que uma transação commitada fez.
#[derive(Debug)]
pub struct Batch {
    /// Identificador da transação.
    pub txn: u64,
    /// Operações, na ordem em que foram executadas.
    pub ops: Vec<WalOp>,
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn encode_batch(txn: u64, ops: &[WalOp]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&txn.to_le_bytes());
    out.extend_from_slice(&(ops.len() as u32).to_le_bytes());
    for op in ops {
        match op {
            WalOp::HeapFormat { page } => {
                out.push(1);
                out.extend_from_slice(&page.to_le_bytes());
            }
            WalOp::HeapLink { prev, next } => {
                out.push(2);
                out.extend_from_slice(&prev.to_le_bytes());
                out.extend_from_slice(&next.to_le_bytes());
            }
            WalOp::HeapInsert { page, slot, data } => {
                out.push(3);
                out.extend_from_slice(&page.to_le_bytes());
                out.extend_from_slice(&slot.to_le_bytes());
                out.extend_from_slice(&(data.len() as u32).to_le_bytes());
                out.extend_from_slice(data);
            }
            WalOp::HeapDelete { page, slot } => {
                out.push(4);
                out.extend_from_slice(&page.to_le_bytes());
                out.extend_from_slice(&slot.to_le_bytes());
            }
        }
    }
    out
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.buf.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
}

fn decode_batch(payload: &[u8]) -> Option<Batch> {
    let mut c = Cursor { buf: payload, pos: 0 };
    let txn = c.u64()?;
    let n = c.u32()? as usize;
    let mut ops = Vec::with_capacity(n.min(1 << 16));
    for _ in 0..n {
        ops.push(match c.u8()? {
            1 => WalOp::HeapFormat { page: c.u32()? },
            2 => WalOp::HeapLink { prev: c.u32()?, next: c.u32()? },
            3 => {
                let page = c.u32()?;
                let slot = c.u16()?;
                let len = c.u32()? as usize;
                WalOp::HeapInsert { page, slot, data: c.take(len)?.to_vec() }
            }
            4 => WalOp::HeapDelete { page: c.u32()?, slot: c.u16()? },
            _ => return None,
        });
    }
    (c.pos == payload.len()).then_some(Batch { txn, ops })
}

/// O arquivo de log. Aberto para escrita no fim; a ordem dos registros é a ordem de commit.
pub struct Wal {
    file: Mutex<File>,
    sync: bool,
}

impl Wal {
    /// Abre o log e devolve os lotes commitados que ele contém. Um final
    /// corrompido é cortado fora.
    pub fn open(path: &Path, sync: bool) -> Result<(Wal, Vec<Batch>)> {
        let mut file =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;

        let (mut batches, mut pos) = (Vec::new(), 0usize);
        while pos + 8 <= bytes.len() {
            let len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
            let crc = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap());
            let Some(payload) = bytes.get(pos + 8..pos + 8 + len) else { break };
            if crc32(payload) != crc {
                break;
            }
            let Some(batch) = decode_batch(payload) else { break };
            batches.push(batch);
            pos += 8 + len;
        }
        if (pos as u64) < bytes.len() as u64 {
            file.set_len(pos as u64)?;
        }
        file.seek(SeekFrom::Start(pos as u64))?;
        Ok((Wal { file: Mutex::new(file), sync }, batches))
    }

    /// Torna a transação durável. A ordem dos registros no arquivo é a ordem de commit.
    pub fn append_commit(&self, txn: u64, ops: &[WalOp]) -> Result<()> {
        let payload = encode_batch(txn, ops);
        let mut rec = Vec::with_capacity(payload.len() + 8);
        rec.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        rec.extend_from_slice(&crc32(&payload).to_le_bytes());
        rec.extend_from_slice(&payload);
        let mut f = self.file.lock();
        f.write_all(&rec)?;
        if self.sync {
            f.sync_data()?;
        }
        Ok(())
    }

    /// Esvazia o log (depois de um checkpoint).
    pub fn reset(&self) -> Result<()> {
        let mut f = self.file.lock();
        f.set_len(0)?;
        f.seek(SeekFrom::Start(0))?;
        f.sync_data()?;
        Ok(())
    }

    /// Tamanho do log em bytes.
    pub fn len(&self) -> Result<u64> {
        Ok(self.file.lock().metadata()?.len())
    }

    /// O log está vazio?
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
}
