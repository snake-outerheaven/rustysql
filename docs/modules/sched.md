# Módulo `sched`

> Gerenciador de prioridade de queries: controle de admissão com fila de prioridade e *aging*.

| | |
|---|---|
| **Arquivo** | `src/sched.rs` |
| **Camada** | motor |
| **Depende de** | `error`; crate `parking_lot` |
| **Usado por** | `ast` e `parser` (tipo `Priority`), `txn` (`Permit`), `db` |
| **Testes** | `tests/sched.rs` (10 testes) + 1 *doctest* |

## 1. Responsabilidade

Latches e locks decidem *quem pode tocar nos dados*. Este módulo decide **quem entra**: no máximo
`max_running` comandos executam ao mesmo tempo e os demais esperam numa fila **ordenada por
prioridade**, sem inanição. Racional: [ADR-0012](../adr/0012-controle-de-admissao-com-prioridade.md).

**Não faz**: proteger dados, nem escolher quais locks pegar.

## 2. API pública

| Item | Descrição |
|---|---|
| `Priority { Low, Normal, High }` | prioridade de sessão (`SET PRIORITY`); `parse`, `name` |
| `Admit { New(Priority), InTransaction, Resume }` | por que se pede a vaga; define o nível de partida |
| `SchedConfig { max_running, aging_step, queue_timeout }` | configuração (padrões: 64, 2 s, 30 s) |
| `Scheduler::new(cfg) -> Arc<Scheduler>` | cria |
| `admit(&Arc<Self>, Admit) -> Permit` | espera a vez e devolve uma vaga |
| `stats() -> SchedStats` | foto dos contadores |
| `Permit::yield_slot()` / `resume()` | devolve / retoma a vaga (o motor usa ao bloquear num lock) |
| `Permit::reason()` | o motivo da admissão |
| `SchedStats { running, queued, admitted[3], admitted_in_txn, resumed, waited, max_wait }` | estatísticas |

## 3. Estruturas de dados

```rust
struct Entry { key: i128, seq: u64 }          // Ord: maior key primeiro; empate: menor seq (FIFO)
struct State {
    running: usize,
    heap: BinaryHeap<Entry>,                  // a fila
    cancelled: HashSet<u64>,                  // desistências ainda no heap
    next_seq: u64,
    stats: SchedStats,
}
pub struct Scheduler { state: Mutex<State>, wake: Condvar, cfg: SchedConfig, epoch: Instant }
pub struct Permit { sched: Arc<Scheduler>, held: bool, why: Admit }   // RAII
```

**Níveis** (`Admit::level`): `Low` 0 · `Normal` 1 · `High` 2 · `InTransaction` 3 · `Resume` 4.

## 4. Algoritmos e fluxos

### A chave de ordenação (o coração do *aging*)

```
chave = nível × aging_step − instante_de_chegada      (em nanossegundos, desde `epoch`)
```

A fila é um max-heap pela chave. Como o tempo anda igual para todos, comparar duas chaves equivale a
comparar `nível×passo − chegada` **em qualquer instante**: **a ordem nunca muda depois de montada**
(não há reordenação), mas quem espera `aging_step` a mais compensa **um nível** de prioridade.

Exemplo (`aging_step = 2 s`): `LOW` chegou em t = 0 → chave 0. `HIGH` chegou em t = 10 s → chave
`2×2 − 10 = −6 s`. A chave do `LOW` é maior: ele passa à frente.

### `acquire`
1. Entra no heap com a chave calculada.
2. Laço: limpa o topo de entradas canceladas; se o **topo é este pedido** e `running < max_running`,
   remove-o, incrementa `running`, atualiza as estatísticas e, se ainda há vaga e fila, acorda os demais.
3. Senão, espera na `Condvar` até o `deadline`; se vencer, marca a entrada como **cancelada** (será
   descartada ao chegar ao topo) e devolve `Error::QueueTimeout`.

### `release`
Decrementa `running` e `notify_all`.

### Integração com o motor (`db.rs`)
* Admissão **por comando**: `Session::run_in_txn` pede a vaga; ela é devolvida no fim do comando (ou
  depois do commit, no autocommit).
* `COMMIT`, `ROLLBACK`, `BEGIN`, `CHECKPOINT` e `SET PRIORITY` **não passam pela fila**.
* `Admit::InTransaction` quando a transação já pegou algum lock (`Txn::locked`).
* **Cessão de vaga**: `Inner::lock` passa um `on_block` ao gerenciador de locks que chama
  `Permit::yield_slot()`; ao ser concedido o lock, `Permit::resume()` pede a vaga de volta com `Admit::Resume`.
* A prioridade da sessão também vira o `rank` da transação (vítima de deadlock).

## 5. Invariantes

1. `running ≤ max_running` sempre (testado: o pico observado é exatamente o limite).
2. O pedido admitido é sempre o de maior chave entre os que esperam.
3. Um pedido cancelado nunca é admitido e não conta em `queued`.
4. Um `Permit` com `held = true` devolve exatamente uma vaga no `Drop`.

## 6. Concorrência

`Scheduler: Send + Sync`; um mutex curto + uma `Condvar` (`notify_all`: *thundering herd*, aceitável).
O escalonador **nunca** chama o `LockManager`; o `LockManager` só o chama, via `on_block`, para
**liberar** uma vaga (nunca para esperar).

## 7. Erros

`Error::QueueTimeout` (transitório) quando a espera passa de `queue_timeout`.

## 8. Testes (`tests/sched.rs`)

| Teste | Garante |
|---|---|
| `higher_priority_enters_first_and_fifo_within_a_level` | `high1, high2, normal1, normal2, low` |
| `transaction_holders_and_resumed_statements_outrank_user_priorities` | `resume > in-txn > high > normal` |
| `aging_lets_a_long_waiting_low_overtake_a_fresh_high` | o `LOW` velho passa o `HIGH` novo; o controle sem espera mantém a prioridade |
| `never_runs_more_than_max_running` | 12 threads × 15 rodadas; pico = 3 = limite; estatísticas por nível |
| `queue_timeout_gives_up_and_does_not_block_those_behind` | desistência não trava ninguém |
| `yielding_a_permit_frees_the_slot_and_resume_waits_for_one` | ceder e retomar |
| `a_statement_blocked_on_a_lock_gives_its_slot_back` | integração: 1 vaga, leitor bloqueado + escritor terminam |
| `set_priority_changes_how_statements_are_admitted` | `SET PRIORITY`, contadores, `in-txn` |
| `deadlock_victim_is_the_lowest_priority_not_the_last_to_ask` | prioridade → vítima de deadlock |
| `with_equal_priority_the_victim_is_the_one_with_less_work_to_lose` | desempate por custo |

## 9. Limitações e evolução

* A prioridade só ordena a **admissão**: depois de entrar, comandos competem pelos locks em pé de
  igualdade (exceto na escolha de vítima). *Priority inheritance* seria o passo seguinte.
* Sem cotas por usuário, sem limite por tabela, sem fila por classe de custo.
* `notify_all` acorda todos a cada liberação.
* `max_running` é fixo; não se adapta à carga.

## 10. Veja também

[ADR-0012](../adr/0012-controle-de-admissao-com-prioridade.md), [`lockmgr`](lockmgr.md), [`db`](db.md),
[`../guides/concurrency-and-transactions.md`](../guides/concurrency-and-transactions.md),
[cenário](../architecture/runtime-scenarios.md#8-admissão-por-prioridade-aging-e-cessão-de-vaga)
