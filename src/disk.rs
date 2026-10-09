//! Disco: leitura/escrita de páginas de 4 KB num arquivo. É a camada mais baixa;
//! quem decide *quando* ir ao disco é o buffer pool.

use crate::error::{Error, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Tamanho de uma página, em bytes. Todo o armazenamento (heap, B-tree, WAL lógico) é
/// organizado em páginas deste tamanho.
pub const PAGE_SIZE: usize = 4096;
/// Número de uma página no arquivo (a página 0 é o cabeçalho). `0` também significa
/// "nenhuma página" nos ponteiros `next` do heap e das folhas da B-tree.
pub type PageId = u32;
/// O conteúdo bruto de uma página.
pub type PageBuf = [u8; PAGE_SIZE];

const MAGIC: &[u8; 8] = b"RUSTYSQ2";

/// Acesso ao arquivo de dados. É a camada mais baixa: só sabe ler e escrever páginas inteiras.
/// Quem decide *quando* ir ao disco é o [`BufferPool`](crate::bufferpool::BufferPool).
pub struct DiskManager {
    file: File,
}

impl DiskManager {
    /// Abre (ou cria) o arquivo; devolve também o número de páginas existentes.
    /// A página 0 é o cabeçalho e nunca passa pelo buffer pool.
    pub fn open(path: &Path) -> Result<(Self, u32)> {
        let mut file =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        let len = file.metadata()?.len();
        if len == 0 {
            let mut page = [0u8; PAGE_SIZE];
            page[..8].copy_from_slice(MAGIC);
            file.write_all(&page)?;
            file.sync_all()?;
            return Ok((DiskManager { file }, 1));
        }
        let mut hdr = [0u8; 8];
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut hdr)?;
        if &hdr != MAGIC {
            return Err(Error::storage(
                "arquivo não é um banco rustysql (ou está em formato antigo; apague-o)",
            ));
        }
        let pages = len.div_ceil(PAGE_SIZE as u64) as u32;
        Ok((DiskManager { file }, pages.max(1)))
    }

    /// Páginas além do fim do arquivo (ainda não gravadas) leem como zeros.
    pub fn read_page(&mut self, id: PageId, buf: &mut PageBuf) -> Result<()> {
        let off = id as u64 * PAGE_SIZE as u64;
        let len = self.file.metadata()?.len();
        if off >= len {
            buf.fill(0);
            return Ok(());
        }
        let avail = ((len - off) as usize).min(PAGE_SIZE);
        self.file.seek(SeekFrom::Start(off))?;
        self.file.read_exact(&mut buf[..avail])?;
        buf[avail..].fill(0);
        Ok(())
    }

    /// Grava a página `id` (que não pode ser a 0). Estende o arquivo, se preciso.
    pub fn write_page(&mut self, id: PageId, buf: &PageBuf) -> Result<()> {
        debug_assert!(id != 0, "a página 0 é reservada");
        self.file.seek(SeekFrom::Start(id as u64 * PAGE_SIZE as u64))?;
        self.file.write_all(buf)?;
        Ok(())
    }

    /// Força os dados gravados até o dispositivo (`fdatasync`).
    pub fn sync(&mut self) -> Result<()> {
        self.file.sync_data()?;
        Ok(())
    }
}
