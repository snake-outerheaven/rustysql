# ADR-0011: Preferência a escritores no gerenciador de locks

* **Status**: aceita
* **Data**: 2026-10-09
* **Módulos afetados**: `lockmgr`

## Contexto e declaração do problema

Num lock leitor/escritor ingênuo, um leitor novo é admitido sempre que **só há leitores** segurando o
lock, mesmo que um escritor já esteja esperando. Com um fluxo contínuo de leitores, sempre há algum
leitor ativo e o escritor **nunca** entra (*writer starvation*).

No teste de transferências, dois auditores executavam `SELECT` em laço apertado (milhões de comandos);
os escritores esperavam indefinidamente.

## Fatores de decisão

* garantir que todo escritor eventualmente entre;
* não punir leitores além do necessário;
* manter o grafo de espera correto (para a detecção de deadlock continuar válida).

## Opções consideradas

1. **Sem preferência** (leitores sempre entram se só há leitores).
2. **Preferência a escritores**: um leitor *novo* não entra enquanto há um escritor esperando o
   lock daquela tabela.
3. **Fila FIFO estrita** de todos os pedidos.
4. **Leitores têm preferência** (invertido): inadequado, pioraria a inanição.

## Resultado da decisão

Escolhida a **opção 2**. `State::writers_waiting: HashMap<TableId, HashSet<TxnId>>` registra os
escritores bloqueados. Em `acquire_with`, um pedido `Shared` de uma transação que **ainda não tem** lock
nessa tabela passa a ser bloqueado também pelos escritores na fila (`blockers.extend(queued)`). Quem já
segura o lock (releitura, ou *upgrade*) não é bloqueado pela fila.

Os escritores da fila entram no grafo de espera como bloqueadores do leitor: um leitor que espera por
um escritor pode participar de um ciclo e ser detectado ([ADR-0010](0010-vitima-de-deadlock.md)).

### Consequências

* **Bom**: nenhum escritor espera indefinidamente por um fluxo de leitores.
* **Bom**: a decisão é local ao gerenciador de locks, sem nova estrutura global.
* **Ruim**: leitores esperam atrás de escritores, mesmo quando o lock está, no instante, só com leitores.
* **Ruim**: não é FIFO estrito: dois escritores na fila não têm ordem entre si (o primeiro a acordar leva).

## Prós e contras das opções

### 1. Sem preferência
* Contras: inanição demonstrada.

### 3. FIFO estrito
* Prós: ordem previsível. Contras: um *upgrade* S→X precisaria furar a fila para evitar deadlock
  consigo mesmo; mais estado e casos especiais.

## Validação

* `txn::concurrent_transfers_conserve_money_and_never_expose_partial_state` roda com dois auditores em
  laço contínuo; termina.
* `txn::readers_wait_for_writers_and_never_see_dirty_data` e `txn::readers_share_the_lock` garantem que
  a preferência não quebrou o compartilhamento entre leitores.

**Observação honesta.** Esta decisão sozinha **não** resolveu a lentidão do teste: ela tratou a inanição
por fluxo de leitores, mas o que dominava era a escolha de vítima de deadlock
([ADR-0010](0010-vitima-de-deadlock.md)). As duas se complementam.

## Links

* [`modules/lockmgr.md`](../modules/lockmgr.md)
