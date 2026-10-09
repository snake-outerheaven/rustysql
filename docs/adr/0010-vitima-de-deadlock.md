# ADR-0010: Vítima de deadlock por prioridade e custo

* **Status**: aceita
* **Data**: 2026-10-09
* **Módulos afetados**: `lockmgr`, `db`, `sched`

## Contexto e declaração do problema

Com locks de tabela em ordens diferentes ([ADR-0009](0009-locks-de-tabela-strict-2pl.md)), duas
transações podem esperar uma pela outra para sempre. O gerenciador monta o **grafo de espera** (quem
espera por quem) e detecta ciclos. Resta decidir **quem aborta** para destravar.

A primeira implementação abortava sempre **quem fechava o ciclo** (o último a pedir o lock): é a
mais simples, porque o requisitante já está executando e basta devolver `Err(Deadlock)`.

### O que deu errado

O teste de transferências bancárias (6 escritores em ordens opostas + 2 "auditores" que somam os saldos)
**travou por mais de 20 segundos**. O diagnóstico, com `LockManager::snapshot`, mostrou:

* os auditores (leitores) travam as duas tabelas em ordem crescente de id;
* metade dos escritores trava em ordem decrescente;
* o ciclo auditor↔escritor é **sempre fechado pelo escritor**, que já fez trabalho e pede o segundo lock
  depois que o auditor entrou na fila;
* logo o escritor era a vítima **toda vez**, tentava de novo e colidia de novo: **inanição**. Medido:
  entre 11 mil e 62 mil *retries* para 240 transferências.

O leitor, que não fez nada, nunca perdia.

## Fatores de decisão

* quem aborta deve ser quem **perde menos**, não quem chegou por último;
* respeitar a prioridade que o usuário deu à sessão (`SET PRIORITY`, [ADR-0012](0012-controle-de-admissao-com-prioridade.md));
* garantir progresso: ao menos uma transação do ciclo precisa terminar.

## Opções consideradas

1. **O requisitante que fecha o ciclo** (inicial).
2. **O mais novo** do ciclo (menor trabalho esperado).
3. **Política por prioridade e custo**: entre **todos os membros do ciclo**, abortar o de menor
   prioridade; empate: o que segura **menos locks exclusivos** (menos trabalho a perder); empate: o
   **mais novo** (maior id).

## Resultado da decisão

Escolhida a **opção 3**. `State::find_cycle` devolve os membros do ciclo e `State::pick_victim` escolhe
`min_by_key((rank, locks_exclusivos, Reverse(id)))`.

* Se a vítima é o **requisitante**: `acquire` retorna `Err(Error::Deadlock)` na hora.
* Se a vítima é **outra transação**, que está dormindo na `Condvar`: o requisitante a inclui em
  `State::victims` e dá `notify_all`; a vítima, ao acordar, vê o seu id em `victims` e retorna
  `Err(Error::Deadlock)`. O erro faz a `Session` abortar a transação **inteira** (rollback e `release_all`),
  o que destrava o requisitante.

Correção associada: `release_all` agora remove a transação que terminou das listas de bloqueadores dos
outros, para que arestas **obsoletas** do grafo de espera não fechem ciclos falsos.

### Consequências

* **Bom**: a vítima é quem perde menos; escritores com trabalho feito vencem leitores ociosos; sessões
  `HIGH` nunca são vítimas de uma `LOW`.
* **Bom**: ordem de grandeza de melhora no cenário medido: de 11 mil–62 mil *retries* (e >20 s de
  espera) para **61 *retries***; a suíte `txn` inteira cai de segundos para ~0,55 s.
* **Ruim**: `exclusive_locks` percorre todos os locks (O(tabelas)); aceitável porque só roda ao
  detectar ciclo.
* **Ruim**: a vítima pode ser uma transação **dormindo** que não fez nada de errado; o cliente precisa
  tratar `Deadlock` como falha transitória e repetir.

## Validação

| Teste | Garante |
|---|---|
| `txn::deadlock_is_detected_and_one_victim_is_chosen` | exatamente uma vítima; o vencedor aplica as duas atualizações |
| `sched::deadlock_victim_is_the_lowest_priority_not_the_last_to_ask` | nas **duas** ordens de chegada (HIGH fecha o ciclo / LOW fecha o ciclo), a vítima é a `LOW` |
| `sched::with_equal_priority_the_victim_is_the_one_with_less_work_to_lose` | com prioridades iguais, perde quem tem menos locks exclusivos |
| `txn::concurrent_transfers_conserve_money_and_never_expose_partial_state` | o cenário que travava agora termina e conserva o dinheiro |

**Mutação.** Trocar `pick_victim` por "o primeiro do ciclo" (o requisitante) faz os dois testes de
`sched` falharem.

## Links

* [`modules/lockmgr.md`](../modules/lockmgr.md),
  [`architecture/runtime-scenarios.md`](../architecture/runtime-scenarios.md#7-deadlock-e-escolha-da-vítima)
