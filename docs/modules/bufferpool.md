# Módulo `bufferpool`

> Cache de páginas com latches, *pin* por `Arc` e política *no-steal*.

| | |
|---|---|
| **Arquivo** | `src/bufferpool.rs` |
| **Camada** | base |
| **Depende de** | `disk`, `error`; crate `parking_lot` |
| **Usado por** | `heap`, `btree`, `db` |
| **Testes** | `btree::*` (todas as árvores vivem num pool), `recovery::small_buffer_pool_evicts_clean_pages_and_checkpoints_automatically`, `recovery::a_transaction_larger_than_the_pool_fails_cleanly_and_the_database_keeps_working` |

## 1. Responsabilidade

* Manter até `capacity` páginas em memória, carregando-as do disco sob demanda.
* Dar a cada página um **latch** (`RwLock`) e um **pin** (contagem de `Arc`).
* Rastrear páginas **sujas** e só gravá-las no [checkpoint](../architecture/runtime-scenarios.md#5-checkpoint).
* Despejar páginas limpas e sem pin quando falta espaço.

**Não faz**: interpretar o conteúdo das páginas; decidir quando fazer checkpoint (é do [`db`](db.md)).

## 2. API pública

| Item | Descrição |
|---|---|
| `BufferPool::open(path, capacity)` | abre o arquivo; `capacity` é no mínimo 16 |
| `fetch(id) -> PageRef` | página `id`, lida do disco se preciso |
| `new_page() -> PageRef` | página nova zerada e **suja**, com o próximo id |
| `ensure_page(id) -> PageRef` | para a recuperação: cria zerada se `id` está além do fim |
| `flush_all()` | grava todas as páginas sujas e faz `fsync` |
| `capacity()`, `dirty_pages()`, `num_pages()` | introspecção |
| `PageRef = Arc<Frame>` | referência fixada (pin) |
| `Frame::read()` / `Frame::write()` | pegam o latch; devolvem os guards abaixo |
| `PageReadGuard`, `PageWriteGuard` | guards *owned*; `Deref<Target = [u8; 4096]>`; o de escrita também `DerefMut` |
| `Frame::id()`, `Guard::id()` | id da página |

## 3. Estruturas de dados

```rust
pub struct Frame {
    id: PageId,
    latch: Arc<RwLock<Box<PageBuf>>>,   // o conteúdo, atrás do latch
    dirty: AtomicBool,                   // modificada desde o último flush?
    referenced: AtomicBool,              // bit do relógio
    dirty_count: Arc<AtomicUsize>,       // contador global (compartilhado)
}
struct State {
    frames: HashMap<PageId, PageRef>,    // tabela de páginas
    clock: VecDeque<PageId>,             // ordem do relógio
    next_page: u32,                      // próximo id a alocar
}
pub struct BufferPool { disk: Mutex<DiskManager>, state: Mutex<State>, capacity, dirty_count }
```

Os guards são `struct { guard: ArcRwLock*Guard<..>, pin: PageRef }`: carregam o `Arc` do frame, então o
**guard mantém a página fixada** e pode ser movido/guardado em vetores (necessário para o *latch
crabbing*; [ADR-0006](../adr/0006-btree-latch-crabbing.md)). A ordem dos campos faz o latch ser solto
antes do pin.

## 4. Algoritmos e fluxos

**`fetch(id)`**: trava `state`; se a página está no mapa, marca `referenced` e devolve; senão chama
`make_room`, lê do disco e insere.

**`make_room`** (relógio / *second chance*): enquanto `frames.len() >= capacity`, percorre `clock`
até `2×len + 1` passos. Uma página é **candidata** se `Arc::strong_count == 1` (só o mapa a segura:
ninguém tem pin) **e** não está suja. Se candidata mas com `referenced = true`, zera o bit e dá outra
chance; se com `referenced = false`, é despejada. Se não acha ninguém, devolve
`Error::Storage("buffer pool cheio de páginas sujas/fixadas: transação grande demais para o pool")`.

**`new_page`**: aloca `next_page`, cria o frame **sujo** (assim nunca é despejado antes do checkpoint) e
incrementa `dirty_count`.

**`flush_all`**: copia a lista de páginas sujas sob `state`, solta `state`, e para cada uma:
`dirty.swap(false)`, pega o latch **de leitura** e escreve no disco; no fim, `sync_data`. Limpar o bit
*antes* de gravar garante que uma modificação concorrente (se houvesse) marcaria a página suja de novo.

**Marcação de sujeira**: `PageWriteGuard::deref_mut` chama `mark_dirty`. Só **modificar** suja a página;
pegar o latch de escrita para olhar, não.

## 5. Invariantes

1. Uma página com `Arc::strong_count > 1` (pin) nunca é despejada.
2. Uma página suja nunca é despejada nem gravada fora de `flush_all` (*no-steal*).
3. `dirty_count` = número de frames com `dirty = true` (atualizado de forma atômica nas transições).
4. `next_page` só cresce.

## 6. Concorrência

* `BufferPool: Send + Sync`.
* **Ordem de locks**: `state` → `disk`. `flush_all` solta `state` antes de pegar `disk` e os latches de
  página.
* O mutex `state` é **curto**: nunca é segurado enquanto se **espera** um latch de página (o
  despejo só consulta `strong_count`, sem tomar latch).
* A leitura do disco numa falta de página acontece com `state` travado: serializa as faltas (aceito
  por simplicidade).
* O pin é verificado sob `state`; como obter um `PageRef` novo também exige `state`, não há corrida
  entre "ver contagem 1" e "alguém pegar a página".

## 7. Erros

`Error::Storage` para id inexistente (`id == 0` ou `≥ next_page`) e para pool cheio; `Error::Io` para disco.

## 8. Testes

* `recovery::small_buffer_pool_evicts_clean_pages_and_checkpoints_automatically`: 3000 linhas num pool de 48 páginas.
* `recovery::a_transaction_larger_than_the_pool_fails_cleanly_and_the_database_keeps_working`: 1000 linhas num pool de 32 páginas falham com "buffer pool" e o banco continua utilizável.
* Todos os testes de `btree` (pool de 16384 páginas, sem `flush`).

## 9. Limitações e evolução

* **No-steal** limita o tamanho de uma transação ao tamanho do pool (D2).
* Relógio simples; sem *scan resistance* (uma varredura grande lava o cache).
* E/S sob o mutex `state`; um pool particionado ou com *latch* por faixa reduziria a contenção.
* Sem pré-busca (*prefetch*) nem gravação em segundo plano.

## 10. Veja também

[`disk`](disk.md), [`btree`](btree.md), [`heap`](heap.md), [ADR-0002](../adr/0002-no-steal-no-force-wal-redo.md), [ADR-0006](../adr/0006-btree-latch-crabbing.md)
