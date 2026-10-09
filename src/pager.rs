//! Pager: o arquivo é um array de páginas de 4 KB.
//! Página 0 = cabeçalho (magic + número de páginas).

use crate::error::{Error, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const PAGE_SIZE: usize = 4096;
pub type PageId = u32;
pub type Page = [u8; PAGE_SIZE];

const MAGIC: &[u8; 8] = b"RUSTYSQL";

pub struct Pager {
    file: File,
    num_pages: u32,
}

impl Pager {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        let len = file.metadata()?.len();
        let mut pager = Pager { file, num_pages: 1 };
        if len == 0 {
            pager.write_header()?;
        } else {
            let mut hdr = [0u8; 12];
            pager.file.seek(SeekFrom::Start(0))?;
            pager.file.read_exact(&mut hdr)?;
            if &hdr[..8] != MAGIC {
                return Err(Error::storage("não é um arquivo rustysql"));
            }
            pager.num_pages = u32::from_le_bytes(hdr[8..12].try_into().unwrap());
        }
        Ok(pager)
    }

    pub fn num_pages(&self) -> u32 {
        self.num_pages
    }

    fn write_header(&mut self) -> Result<()> {
        let mut page = [0u8; PAGE_SIZE];
        page[..8].copy_from_slice(MAGIC);
        page[8..12].copy_from_slice(&self.num_pages.to_le_bytes());
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&page)?;
        Ok(())
    }

    pub fn read(&mut self, id: PageId) -> Result<Box<Page>> {
        if id >= self.num_pages {
            return Err(Error::storage(format!("página {id} fora do arquivo")));
        }
        let mut page = Box::new([0u8; PAGE_SIZE]);
        self.file.seek(SeekFrom::Start(id as u64 * PAGE_SIZE as u64))?;
        self.file.read_exact(&mut page[..])?;
        Ok(page)
    }

    pub fn write(&mut self, id: PageId, page: &Page) -> Result<()> {
        if id == 0 || id >= self.num_pages {
            return Err(Error::storage(format!("escrita inválida na página {id}")));
        }
        self.file.seek(SeekFrom::Start(id as u64 * PAGE_SIZE as u64))?;
        self.file.write_all(page)?;
        Ok(())
    }

    /// Acrescenta uma página zerada ao fim do arquivo.
    pub fn allocate(&mut self) -> Result<PageId> {
        let id = self.num_pages;
        self.file.seek(SeekFrom::Start(id as u64 * PAGE_SIZE as u64))?;
        self.file.write_all(&[0u8; PAGE_SIZE])?;
        self.num_pages += 1;
        self.write_header()?;
        Ok(id)
    }
}
