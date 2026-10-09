# ADR-0012: Controle de admissão com prioridade, *aging* e cessão de vaga

* **Status**: aceita
* **Data**: 2026-10-09
* **Módulos afetados**: `sched`, `db`, `lockmgr`, `txn`, `parser`

## Contexto e declaração do problema

Latches e locks decidem **quem pode tocar nos dados**. Mas não decidem **quem entra**: se centenas de
comandos chegam ao mesmo tempo, todos competem ao mesmo tempo, na ordem que o sistema operacional
quiser, e uma consulta urgente fica na mesma fila que um relatório pesado. *Proteger o acesso não
basta: é preciso ordenar a entrada.*

Requisitos:

* limitar quantos comandos executam ao mesmo tempo;
* os demais esperam numa fila **ordenada por prioridade**;
* prioridade não pode causar **inanição**: um comando `LOW` precisa, eventualmente, entrar;
* não pode causar **deadlock de admissão**: comandos esperando um lock não podem impedir que o dono do
  lock avance.

## Fatores de decisão

* ordem de entrada previsível e testável;
* justiça com o tempo de espera (anti-inanição);
* integração com locks e transações (quem já segura locks deve terminar logo);
* implementação pequena.

## Opções consideradas

1. **Sem controle de admissão** (só locks).
2. **Pool de vagas FIFO** (semáforo): limita a concorrência, mas ignora prioridade.
3. **Fila de prioridade estrita** (`HIGH` sempre antes de `NORMAL`, antes de `LOW`).
4. **Fila de prioridade com *aging***: a prioridade efetiva cresce com o tempo de espera.

## Resultado da decisão

Escolhida a **opção 4**, em `sched.rs`.

### Ordem da fila

Cada pedido recebe uma chave **fixa**, calculada na chegada:

```
chave = nível × aging_step − instante_de_chegada
```

e a fila é um `BinaryHeap` (maior chave primeiro; empate: quem chegou antes, ou seja, FIFO por nível).
O tempo anda igual para todos, então comparar duas chaves equivale a comparar `nível×passo − chegada`
**em qualquer instante**: a ordem nunca muda depois de montada (nenhuma reordenação), mas quem espera
`aging_step` a mais compensa **um nível** de diferença. Com `aging_step = 2 s`, um `LOW` que esperou
mais de 4 s passa à frente de um `HIGH` recém-chegado.

| Nível | Quem |
|---|---|
| 0 / 1 / 2 | `LOW` / `NORMAL` / `HIGH` (`SET PRIORITY`) |
| 3 | `InTransaction`: comando dentro de uma transação **explícita que já segura locks** |
| 4 | `Resume`: comando que cedeu a vaga ao bloquear num lock e agora volta |

### Regras

1. **Máximo de `max_running` comandos** executando (padrão 64).
2. **Admissão por comando**, não por transação: uma transação explícita ociosa não segura vaga.
3. **`COMMIT`, `ROLLBACK`, `BEGIN`, `CHECKPOINT` e `SET PRIORITY` não passam pela fila**: liberam
   recursos, não consomem.
4. **Quem já segura locks entra na frente** (nível 3): segurar um lock enquanto espera uma vaga só
   atrasa todo mundo (inversão de prioridade).
5. **Cessão de vaga ao bloquear**: o `LockManager::acquire_with` chama `on_block` antes da primeira
   espera de verdade; o motor devolve a vaga (`Permit::yield_slot`) e a retoma com nível 4
   (`Permit::resume`) quando o lock é concedido. Sem isso, com `max_running = 1`, um leitor bloqueado
   por um escritor ocuparia a única vaga e o escritor nunca poderia executar o próximo comando para
   terminar: **deadlock de admissão**.
6. `begin()` roda **antes** da admissão: ele pode esperar um checkpoint, e o checkpoint espera as
   transações ativas; quem espera na fila de admissão não pode segurar uma vaga de que elas dependam.
7. **Timeout de fila** (`queue_timeout`, padrão 30 s): `Error::QueueTimeout`. A desistência deixa uma
   entrada "cancelada" no heap, descartada quando chega ao topo, e não conta como fila.
8. A prioridade da **sessão** também vira o `rank` da transação para escolher vítimas de deadlock
   ([ADR-0010](0010-vitima-de-deadlock.md)).

### Consequências

* **Bom**: ordem de entrada explícita, configurável (`SET PRIORITY`) e observável (`Database::scheduler_stats`).
* **Bom**: sem inanição (aging) e sem deadlock de admissão (cessão de vaga).
* **Bom**: **uma só estrutura** (heap + `Condvar`), sem threads de fundo.
* **Ruim**: a prioridade vale só para a **admissão**; depois de entrar, comandos competem pelos locks
  em pé de igualdade (exceto na escolha de vítima de deadlock). *Priority inheritance* nos locks seria
  um passo futuro.
* **Ruim**: o `notify_all` acorda todos os esperadores a cada liberação (*thundering herd*); aceitável
  para dezenas de esperadores, não para milhares.
* **Ruim**: o nível 3 é "tudo ou nada" por sessão (não considera quantos locks a transação segura).

## Prós e contras das opções

### 1. Sem controle
* Contras: sem como priorizar; sobrecarga sem limite.

### 2. Semáforo FIFO
* Prós: trivial. Contras: ignora prioridade; é exatamente o que o requisito pede para evitar.

### 3. Prioridade estrita
* Contras: um fluxo contínuo de `HIGH` esfomeia `LOW` para sempre.

## Validação (`tests/sched.rs`)

| Teste | Garante |
|---|---|
| `higher_priority_enters_first_and_fifo_within_a_level` | `high1, high2, normal1, normal2, low` |
| `transaction_holders_and_resumed_statements_outrank_user_priorities` | `resume > in-txn > high > normal` |
| `aging_lets_a_long_waiting_low_overtake_a_fresh_high` | o `LOW` velho passa à frente do `HIGH` novo; com pausa curta, a prioridade manda |
| `never_runs_more_than_max_running` | o pico de concorrência é exatamente `max_running`, nunca mais |
| `queue_timeout_gives_up_and_does_not_block_those_behind` | a desistência não trava a fila |
| `yielding_a_permit_frees_the_slot_and_resume_waits_for_one` | ceder e retomar a vaga |
| `a_statement_blocked_on_a_lock_gives_its_slot_back` | com 1 vaga, leitor bloqueado + escritor terminam (sem deadlock de admissão) |
| `set_priority_changes_how_statements_are_admitted` | `SET PRIORITY` e contadores por nível |

## Links

* [`modules/sched.md`](../modules/sched.md), [`guides/concurrency-and-transactions.md`](../guides/concurrency-and-transactions.md),
  [`architecture/runtime-scenarios.md`](../architecture/runtime-scenarios.md#8-admissão-por-prioridade-aging-e-cessão-de-vaga)
