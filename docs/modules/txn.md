# Módulo `txn`

> Estado de uma transação em andamento: WAL pendente, log de undo e os recursos que ela segura.

| | |
|---|---|
| **Arquivo** | `src/txn.rs` |
| **Camada** | motor |
| **Depende de** | `btree`, `heap`, `lockmgr`, `sched`, `wal`, `error`; `parking_lot` |
| **Usado por** | `db` |
| **Testes** | `txn::*` (8 testes), `recovery::pages_allocated_by_a_rolled_back_transaction_still_work_after_recovery` |

## 1. Responsabilidade

Acumular, durante a transação, **(1)** as operações de WAL que serão gravadas de uma vez no commit e
**(2)** as ações de **undo** em memória para o rollback; e **ser dona** dos recursos que a transação
segura, liberando-os quando ela é descartada.

**Não faz**: decidir *quando* commitar ou desfazer (é do [`db`](db.md)), nem tomar locks (os locks ficam
no [`lockmgr`](lockmgr.md), identificados pelo `id`).

## 2. API pública

| Item | Descrição |
|---|---|
| `struct Txn` | a transação (campos públicos para o motor) |
| `Undo { HeapInsert, HeapDelete, IndexInsert, IndexDelete }` | uma ação de desfazer |
| `Txn::mark() -> Mark` | ponto de salvamento (tamanhos de `undo` e `wal`) |
| `Txn::rollback_to(mark)` | desfaz tudo depois da marca |
| `Mark::start()` | marca zero: desfaz a transação inteira |

## 3. Estruturas de dados

| Campo de `Txn` | Papel |
|---|---|
| `id: TxnId` | identificador (usado nos locks e no WAL) |
| `explicit: bool` | aberta por `BEGIN` (DDL é recusado) |
| `priority: Priority` | prioridade da sessão; vira o `rank` para vítima de deadlock |
| `wal: Vec<WalOp>` | operações de redo pendentes |
| `undo: Vec<Undo>` | ações inversas, em ordem de execução |
| `on_commit: Vec<Box<dyn FnOnce() + Send>>` | publicações de DDL executadas **depois** do commit durável |
| `_gate: ArcRwLockReadGuard<()>` | mantém o **portão de checkpoint** aberto enquanto a transação existe |
| `ddl_guard: Option<ArcMutexGuard<()>>` | serializa DDL |
| `permit: Option<Permit>` | a vaga de execução do comando em andamento |
| `locked: bool` | já pegou algum lock (os próximos comandos entram na frente na fila) |

`Undo`:

| Variante | Desfaz | Como |
|---|---|---|
| `HeapInsert{heap, rid}` | uma inserção | `heap.delete(rid)` |
| `HeapDelete{heap, rid, data}` | uma remoção | `heap.put_at(rid, data)` (**mesmo** slot) |
| `IndexInsert{tree, key}` | uma entrada de índice | `tree.delete(key)` |
| `IndexDelete{tree, key, rid}` | uma remoção de índice | `tree.insert(key, rid)` |

## 4. Algoritmos e fluxos

**`rollback_to(mark)`**:
1. Enquanto `undo.len() > mark.undo`: tira a última ação e a aplica. Erros não interrompem: guarda o
   primeiro e **continua** (melhor desfazer o máximo possível).
2. Separa `wal` em `[..mark.wal]` (fica) e a cauda; da cauda, **só as operações estruturais**
   (`HeapFormat`, `HeapLink`) voltam para `wal` ([ADR-0005](../adr/0005-operacoes-estruturais-sobrevivem-ao-rollback.md)).

**Ciclo de vida** (feito em `db.rs`): `begin` cria o `Txn` (pega o portão) → comandos acrescentam a
`wal`/`undo` → `commit` grava `wal` e executa `on_commit`, ou `rollback` aplica `rollback_to(Mark::start())`
e grava as operações estruturais que sobraram → ao sair de escopo, o `Txn` solta portão, guard de DDL e
vaga.

## 5. Invariantes

1. `undo` e `wal` só crescem durante um comando; só `rollback_to` os encurta.
2. Depois de `rollback_to`, `wal` contém **apenas** operações que devem sobreviver (as de antes da marca e
   as estruturais).
3. Os recursos do `Txn` (portão, guard de DDL, vaga) são liberados por RAII, mesmo em caso de pânico/`?`.

## 6. Concorrência

Uma transação é usada por **uma thread por vez** (via a `Session`). Os guards usam `parking_lot` com o
recurso `send_guard`, então `Txn` (e a `Session`) pode ser movido entre threads.

## 7. Erros

`rollback_to` devolve o primeiro erro de undo (E/S ou pool cheio ao restaurar uma página).

## 8. Testes

`txn::rollback_and_commit`, `txn::a_failing_statement_does_not_abort_the_transaction`,
`txn::dropping_a_session_rolls_back`, `sql::unique_index_and_statement_atomicity`,
`recovery::pages_allocated_by_a_rolled_back_transaction_still_work_after_recovery`.

## 9. Limitações e evolução

* O undo é só em memória: transações maiores que a memória não cabem (D2).
* Sem *savepoints* nomeados (`SAVEPOINT x`): só a marca implícita por comando.
* Se o próprio undo falha, o estado fica inconsistente.

## 10. Veja também

[ADR-0005](../adr/0005-operacoes-estruturais-sobrevivem-ao-rollback.md), [ADR-0013](../adr/0013-rollback-em-memoria-e-atomicidade-por-comando.md),
[`db`](db.md), [`wal`](wal.md), [cenário de rollback](../architecture/runtime-scenarios.md#3-rollback)
