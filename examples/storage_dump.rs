//! Mostra os bytes reais que o rustysql grava em disco.
//!
//! Cria um banco minúsculo, executa alguns comandos e imprime (em hexadecimal) o WAL logo depois do
//! commit e, depois de um checkpoint, a página de cabeçalho, a raiz do catálogo, o heap da tabela e
//! a folha da B-tree da chave primária. É a fonte dos exemplos em `docs/reference/storage-formats.md`.
//!
//! ```text
//! cargo run --example storage_dump
//! ```

use rustysql::Database;
use std::path::Path;

const PAGE: usize = 4096;

/// `xxd` simplificado: deslocamento, 16 bytes em hex e a coluna ASCII.
fn hexdump(bytes: &[u8], base: usize) {
    for (i, chunk) in bytes.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String =
            chunk.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }).collect();
        println!("  {:04x}  {:<47}  |{ascii}|", base + i * 16, hex.join(" "));
    }
}

fn page(file: &[u8], id: usize) -> &[u8] {
    &file[id * PAGE..(id + 1) * PAGE]
}

fn dump_page(file: &[u8], id: usize, what: &str, regions: &[(usize, usize)]) {
    println!("\n── página {id}: {what}");
    let p = page(file, id);
    for &(from, to) in regions {
        println!("  [bytes {from}..{to}]");
        hexdump(&p[from..to], from);
    }
}

fn main() -> rustysql::Result<()> {
    let path = std::env::temp_dir().join("rustysql_storage_dump.db");
    let wal_path = format!("{}.wal", path.display());
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&wal_path);

    let db = Database::open(&path)?;
    db.run("CREATE TABLE t (id INT PRIMARY KEY, nome TEXT)")?;
    let after_create = db.wal_bytes();
    db.run("INSERT INTO t VALUES (1, 'ana'), (2, 'bia')")?;
    let wal = std::fs::read(&wal_path)?;

    println!("== WAL ({} bytes: CREATE TABLE = {after_create}, INSERT = {})", wal.len(), wal.len() as u64 - after_create);
    println!("\n── registro do CREATE TABLE");
    hexdump(&wal[..after_create as usize], 0);
    println!("\n── registro do INSERT");
    hexdump(&wal[after_create as usize..], after_create as usize);

    db.checkpoint()?;
    drop(db);
    let file = std::fs::read(Path::new(&path))?;
    println!("\n== arquivo de dados ({} páginas de {PAGE} bytes)", file.len() / PAGE);

    dump_page(&file, 0, "cabeçalho (magic)", &[(0, 16)]);
    dump_page(&file, 1, "raiz do catálogo (heap)", &[(0, 24), (PAGE - 160, PAGE)]);
    dump_page(&file, 2, "heap da tabela t", &[(0, 24), (PAGE - 48, PAGE)]);
    dump_page(&file, 3, "folha da B-tree pk_t", &[(0, 48)]);
    Ok(())
}
