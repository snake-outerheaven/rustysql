# Módulo `lockmgr`

> Locks de tabela (compartilhado/exclusivo), *strict 2PL*, detecção de deadlock e escolha de vítima.

| | |
|---|---|
| **Arquivo** | `src/lockmgr.rs` |
| **Camada** | motor |
| **Depende de** | `error`; crate `parking_lot` |
| **Usado por** | `catalog` (tipo `TableId`), `txn` (tipo `TxnId`), `db` |
| **Testes** | `txn::deadlock_is_detected_and_one_victim_is_chosen`, `txn::readers_*`, `txn::concurrent_transfers_*`, `sched::deadlock_victim_*`, `sched::with_equal_priority_*` |

## 1. Responsabilidade

Decidir **quem pode usar uma tabela agora**: conceder, bloquear ou recusar (deadlock/timeout) pedidos
de lock, e soltar tudo no fim da transação. Isto é o **isolamento lógico** (a camada "quem pode tocar
nos dados"); o isolamento **físico** é feito pelos latches de página.

**Não faz**: locks de linha, de chave ou de intenção; nem ordenar a *entrada* de comandos (é do
[`sched`](sched.md)).

## 2. API pública

| Item | Descrição |
|---|---|
| `LockManager::new(timeout)` | cria o gerenciador |
| `acquire(txn, table, mode)` | pega o lock; espera se preciso |
| `acquire_with(txn, table, mode, rank, on_block)` | idem, informando a **prioridade** (`rank`) da transação e um *callback* chamado **uma vez** antes da primeira espera de verdade |
| `release_all(txn)` | solta todos os locks da transação e acorda os esperadores |
| `snapshot() -> String` | foto legível: quem segura, quem espera por quem, fila de escritores |
| `LockMode { Shared, Exclusive }` | modos |
| `TxnId = u64`, `TableId = u32`, `Rank = u8` | tipos |

## 3. Estruturas de dados

```rust
struct State {
    holders: HashMap<TableId, HashMap<TxnId, LockMode>>,   // quem segura o quê
    waiting: HashMap<TxnId, Vec<TxnId>>,                   // grafo de espera: A espera por [B, C]
    writers_waiting: HashMap<TableId, HashSet<TxnId>>,     // fila de escritores por tabela
    rank: HashMap<TxnId, Rank>,                            // prioridade da transação
    victims: HashSet<TxnId>,                               // vítimas de deadlock ainda dormindo
}
pub struct LockManager { state: Mutex<State>, released: Condvar, timeout: Duration }
```

**Compatibilidade**: `Shared` com `Shared` ✓; qualquer combinação com `Exclusive` ✗. Uma transação
com `Shared` que pede `Exclusive` faz um **upgrade** (só passa se for a única a segurar).

## 4. Algoritmos e fluxos

`acquire_with` roda num laço sob o mutex de `state`:

1. Se a transação foi marcada em `victims` → `Err(Deadlock)`.
2. Calcula o modo desejado (já tem um modo suficiente → `Ok`).
3. Calcula os **bloqueadores**: outros donos incompatíveis; se o pedido é `Shared`, **também** os
   escritores na fila daquela tabela ([ADR-0011](../adr/0011-preferencia-a-escritores.md)).
4. Sem bloqueadores → concede.
5. Com bloqueadores: se o pedido é `Exclusive`, entra em `writers_waiting`; registra as arestas em
   `waiting`; **procura um ciclo** a partir dele (`find_cycle`, DFS).
   * **Ciclo**: `pick_victim` escolhe, entre os membros, o de menor `(rank, nº de locks exclusivos, −id)`.
     Se for o requisitante → `Err(Deadlock)`; senão marca em `victims` e dá `notify_all`.
   * Na 1ª espera, chama `on_block()` (o motor devolve a vaga de execução).
   * Espera na `Condvar` até o `deadline` (`Err(LockTimeout)` se vencer) e volta ao passo 1.
6. Ao sair (com sucesso ou erro): remove a transação de `waiting` e de `writers_waiting`; em caso de
   erro, `notify_all` (quem esperava atrás dela pode ter ficado livre).

`release_all`: remove a transação de `holders`, `waiting` (e das listas dos outros: **sem arestas
obsoletas**), `rank` e `victims`; `notify_all`.

## 5. Invariantes

1. Uma tabela com um dono `Exclusive` não tem nenhum outro dono.
2. Toda transação em `waiting` está de fato bloqueada ou prestes a reavaliar (arestas de quem terminou
   são removidas em `release_all`).
3. Se um ciclo existe quando alguém vai esperar, uma vítima é escolhida **antes** de dormir: não há
   deadlock que dure mais que uma rodada de detecção.
4. O **timeout** é só uma rede de segurança.

## 6. Concorrência

* Um único mutex (`state`) e uma `Condvar` compartilhada: simples e correto; `notify_all` causa
  *thundering herd* (aceitável em dezenas de transações).
* **Ordem de aquisição entre módulos**: `LockManager.state` → `Scheduler.state` (só via `on_block`, que
  *libera* uma vaga e nunca espera). O escalonador **nunca** chama o gerenciador de locks.
* A vítima, se for outra transação, falha **ao acordar**; só então o `Session` faz o rollback e
  `release_all` destrava o ciclo.

## 7. Erros

`Error::Deadlock`, `Error::LockTimeout`. Ambos são **transitórios**: o motor aborta a transação inteira
e o cliente deve repetir.

## 8. Testes

| Teste | Garante |
|---|---|
| `txn::readers_wait_for_writers_and_never_see_dirty_data` | exclusão leitor/escritor |
| `txn::readers_share_the_lock` | compartilhamento |
| `txn::deadlock_is_detected_and_one_victim_is_chosen` | exatamente uma vítima |
| `sched::deadlock_victim_is_the_lowest_priority_not_the_last_to_ask` | política por prioridade (2 ordens de chegada) |
| `sched::with_equal_priority_the_victim_is_the_one_with_less_work_to_lose` | desempate por custo |
| `txn::concurrent_transfers_conserve_money_and_never_expose_partial_state` | estresse: sem inanição nem travamento |

## 9. Limitações e evolução

* Granularidade de **tabela** (D1). Locks de linha/chave e de intenção (IS/IX) permitiriam concorrência
  entre escritores da mesma tabela.
* A fila de escritores não é FIFO estrito.
* Uma fila de espera por lock com *priority inheritance* (herdar a prioridade de quem espera) não existe.
* `snapshot()` é para diagnóstico, não tem formato estável.

## 10. Veja também

[ADR-0009](../adr/0009-locks-de-tabela-strict-2pl.md), [ADR-0010](../adr/0010-vitima-de-deadlock.md), [ADR-0011](../adr/0011-preferencia-a-escritores.md),
[`sched`](sched.md), [`db`](db.md) (`Inner::lock`), [cenário de deadlock](../architecture/runtime-scenarios.md#7-deadlock-e-escolha-da-vítima)
