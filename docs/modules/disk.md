# Módulo `disk`

> Lê e grava páginas de 4 KB num arquivo. A camada mais baixa.

| | |
|---|---|
| **Arquivo** | `src/disk.rs` |
| **Camada** | base |
| **Depende de** | `error` |
| **Usado por** | `bufferpool`, `heap`, `btree`, `wal` (só pelos tipos `PageId`/`PAGE_SIZE`) |
| **Testes** | indireto: toda persistência (`basic::persistence_and_many_pages`, `recovery::*`) |

## 1. Responsabilidade

* Abrir/criar o arquivo de dados e validar o cabeçalho.
* Ler uma página inteira por id e gravar uma página inteira por id.
* Forçar os dados ao dispositivo (`fdatasync`).

**Não faz**: cache, decidir quando gravar, ou saber o que há dentro de uma página (isso é do
[`bufferpool`](bufferpool.md) e de quem usa as páginas).

## 2. API pública

| Item | Descrição |
|---|---|
| `PAGE_SIZE: usize = 4096` | tamanho de uma página |
| `type PageId = u32` | número de página; `0` = cabeçalho e também "nenhuma página" nos ponteiros |
| `type PageBuf = [u8; PAGE_SIZE]` | conteúdo de uma página |
| `DiskManager::open(path) -> (DiskManager, u32)` | abre/cria; devolve o nº de páginas existentes |
| `DiskManager::read_page(id, &mut buf)` | lê (zeros para páginas além do fim do arquivo) |
| `DiskManager::write_page(id, &buf)` | grava na posição `id × 4096` |
| `DiskManager::sync()` | `sync_data` |

## 3. Estruturas de dados

**Cabeçalho (página 0).** Os 8 primeiros bytes são o *magic* `b"RUSTYSQ2"`; o resto é zero. Um arquivo
cujo início não é esse magic (inclusive o formato antigo `RUSTYSQL`) é rejeitado com
`Error::Storage("arquivo não é um banco rustysql (ou está em formato antigo; apague-o)")`.

O número de páginas **não** fica no cabeçalho: é `ceil(tamanho_do_arquivo / 4096)`, calculado na
abertura. O contador `next_page` do [`bufferpool`](bufferpool.md) cobre as páginas ainda não gravadas.

## 4. Algoritmos e fluxos

* **Abertura de arquivo vazio**: escreve o cabeçalho (4096 bytes), `sync_all`, devolve 1 página.
* **`read_page`**: se `id × 4096 ≥ tamanho do arquivo`, preenche com zeros (uma página alocada em
  memória mas nunca gravada lê como zerada); se a leitura cobre só parte do arquivo, completa com zeros.
* **`write_page`**: `seek` + `write_all`; o sistema operacional estende o arquivo (com buracos zerados
  para páginas intermediárias que nunca foram gravadas).

## 5. Invariantes

* A página 0 nunca é gravada pelo pool (`debug_assert!(id != 0)`).
* O tamanho do arquivo é sempre múltiplo de 4096 depois de gravações de páginas inteiras.

## 6. Concorrência

`DiskManager` **não** é sincronizado: quem o usa ([`bufferpool`](bufferpool.md)) o envolve num `Mutex`
(`BufferPool::disk`). Ordem de locks: `state` do pool → `disk`.

## 7. Erros

`Error::Io` para qualquer falha do SO; `Error::Storage` para magic inválido.

## 8. Testes

Sem teste unitário próprio: é só E/S. Coberto por todo teste que reabre um banco.

## 9. Limitações e evolução

* **Torn pages**: a escrita de 4 KB não é garantida atômica; o redo idempotente do WAL mitiga mas não
  elimina (risco R1 do arc42). Um *double-write buffer* resolveria.
* Sem E/S direta (`O_DIRECT`), sem leitura antecipada, sem `pwrite`/`pread` (usa `seek` + `read/write`).
* Sem checksum por página.

## 10. Veja também

[`bufferpool`](bufferpool.md), [`../reference/storage-formats.md`](../reference/storage-formats.md#arquivo-de-dados), [ADR-0001](../adr/0001-paginas-4kb-slotted-pages.md)
