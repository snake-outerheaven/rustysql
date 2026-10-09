# Módulo `heap`

> Registros de tamanho variável em *slotted pages*, encadeadas por tabela.

| | |
|---|---|
| **Arquivo** | `src/heap.rs` |
| **Camada** | armazenamento |
| **Depende de** | `bufferpool`, `disk`, `error`, `wal` |
| **Usado por** | `btree` (tipo `RowId`), `catalog`, `exec`, `keyenc` (`RowId`), `txn`, `db` |
| **Testes** | `basic::persistence_and_many_pages`, `recovery::replaying_the_log_twice_gives_the_same_result`, `recovery::pages_allocated_by_a_rolled_back_transaction_still_work_after_recovery` |

## 1. Responsabilidade

Guardar, achar e apagar registros (as linhas serializadas de uma tabela) dentro de páginas, e
fornecer a varredura. Registrar o que fez numa lista de `WalOp` entregue pelo chamador.

**Não faz**: saber o que é uma linha (os bytes são opacos), índices, transações ou locks.

## 2. API pública

| Item | Descrição |
|---|---|
| `RowId { page, slot }` | endereço físico; `Copy`, `Hash`, `Ord` |
| `MAX_RECORD` | 4084 bytes (página − cabeçalho − 1 slot) |
| `HeapFile::open(pool, root)` | handle para um heap existente |
| `HeapFile::create(&pool, &mut log)` | novo heap com 1 página (registra `HeapFormat`) |
| `insert(&self, data, &mut log) -> RowId` | insere; aloca página nova se todas estão cheias |
| `get(rid) -> Option<Vec<u8>>` | lê; `None` se o slot está livre |
| `delete(rid, &mut log) -> Vec<u8>` | apaga; devolve o conteúdo antigo (para o undo) |
| `put_at(rid, data, &mut log)` | recoloca no **mesmo** slot (desfaz um delete) |
| `scan() -> HeapScan` | iterador preguiçoso `(RowId, Vec<u8>)`, uma página por vez |
| `apply_op(&pool, &WalOp)` | reaplica uma operação do WAL (recuperação) |

`HeapFile: Clone` barato (o `hint` é um `Arc<AtomicU32>` compartilhado).

## 3. Estruturas de dados

**Página de heap** (4096 bytes):

```
0      4      6      8                                  free_end            4096
┌──────┬──────┬──────┬─────────────────────────┬ ─ ─ ─ ─ ─ ┬────────────────┐
│ next │nslots│free_ │ slot0 slot1 ... slotN-1 │  livre    │ registros ← ←  │
│ u32  │ u16  │end u16│ (offset u16, len u16)   │           │                │
└──────┴──────┴──────┴─────────────────────────┴ ─ ─ ─ ─ ─ ┴────────────────┘
```

* `next`: próxima página da tabela (`0` = fim).
* `offset == 0` marca um **slot livre** (nenhum registro começa antes do byte 8).
* Registros crescem do fim da página para o começo; o vetor de slots, do começo para o fim.

## 4. Algoritmos e fluxos

**`insert`**: começa na página `hint` (primeira que pode ter espaço) e, para cada página: pega o latch de
escrita; se `can_insert` (cabe reaproveitando slot livre ou acrescentando um), insere e atualiza o
`hint`; senão segue `next`; no fim da cadeia, aloca `new_page`, formata-a, encadeia (`HeapFormat` +
`HeapLink`) e tenta de novo. O `delete` baixa o `hint` (`fetch_min`).

**Compactação** (`page::compact`): quando há espaço **total** suficiente mas o espaço **contíguo** não,
reescreve os registros vivos colados no fim da página, preservando os índices de slot. Só roda quando
precisa (dentro de `reserve`).

**`put_at(slot, data)`** (redo e undo): estende o vetor de slots se preciso (slots novos nascem livres);
se o slot já tem exatamente esse conteúdo, não faz nada (**idempotente**); senão libera o antigo e
reserva espaço novo. Retorna `false` se não há espaço (vira `Error::Storage` no chamador).

**`apply_op`** (recuperação), todas idempotentes:

| Operação | Efeito |
|---|---|
| `HeapFormat{page}` | `ensure_page` + inicializa como página vazia |
| `HeapLink{prev,next}` | `prev.next = next` (garante ambas as páginas) |
| `HeapInsert{page,slot,data}` | `put_at` |
| `HeapDelete{page,slot}` | libera o slot **se** estiver ocupado |

**`HeapScan`**: para cada página, pega o latch de leitura, copia todos os registros vivos para um
buffer, solta o latch e segue `next`. **Não segura latch entre páginas.**

## 5. Invariantes

1. Um `RowId` válido nunca muda de endereço enquanto o registro existe (compactar só move bytes).
2. `free_end ≥ 8 + nslots × 4` (o vetor de slots nunca invade a área de registros).
3. A cadeia `next` é acíclica e os ids crescem ao longo dela (páginas novas vão para o fim).
4. `hint` ≤ qualquer página com espaço livre (pode ser conservador, nunca otimista).

## 6. Concorrência

* Cada operação pega o **latch de escrita da página** (leitura, para `get` e `scan`).
* **Um escritor por tabela** é garantido pelo lock lógico (`Exclusive`; [ADR-0009](../adr/0009-locks-de-tabela-strict-2pl.md)),
  então não há disputa entre escritores pela mesma página; os latches protegem contra os leitores de
  outras threads (índices lendo `get`).
* `HeapScan` tolera páginas novas anexadas durante a varredura (lê `next` sob o latch).
* O heap do **catálogo** (página 1) é protegido pelo guard de DDL (um DDL por vez).

## 7. Erros

`Error::Exec` para registro vazio ou maior que `MAX_RECORD`; `Error::Storage` para registro inexistente
em `delete` e para falta de espaço em `put_at`; erros do pool propagam.

## 8. Testes

* `basic::persistence_and_many_pages`: 500 linhas em várias páginas, apaga 250, reinsere (reuso de slots).
* `recovery::replaying_the_log_twice_gives_the_same_result`: 60 linhas, apaga e altera; reaplicar o WAL duas vezes dá o mesmo.
* `recovery::pages_allocated_by_a_rolled_back_transaction_still_work_after_recovery`: encadeamento sobrevivendo ao rollback.

## 9. Limitações e evolução

* Sem páginas de *overflow*: linha > 4084 bytes é rejeitada.
* `DROP TABLE` não devolve páginas (D4); não há lista de páginas livres.
* Varredura copia todos os registros de uma página (simples; não é *zero-copy*).
* `can_insert` e `insert` calculam `live_bytes` percorrendo os slots: O(slots) por inserção.

## 10. Veja também

[`bufferpool`](bufferpool.md), [`wal`](wal.md), [`txn`](txn.md), [ADR-0001](../adr/0001-paginas-4kb-slotted-pages.md), [ADR-0005](../adr/0005-operacoes-estruturais-sobrevivem-ao-rollback.md), [`../reference/storage-formats.md`](../reference/storage-formats.md#página-de-heap)
