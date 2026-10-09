# ADR-0013: Rollback em memória e atomicidade por comando

* **Status**: aceita
* **Data**: 2026-10-09
* **Módulos afetados**: `txn`, `db`

## Contexto e declaração do problema

Um comando pode falhar no meio: um `INSERT` de três linhas insere duas e a terceira viola a chave
primária. Uma transação também pode desistir (`ROLLBACK`) ou ser abortada (deadlock). Nos dois casos, o
que já foi feito nas páginas em memória precisa ser desfeito. E, dentro de uma transação explícita, uma
questão de semântica: o erro de **um comando** deve abortar a transação **inteira**?

## Fatores de decisão

* o arquivo de dados nunca tem estados não commitados ([ADR-0002](0002-no-steal-no-force-wal-redo.md)),
  então o undo **não precisa ir para o disco**;
* um comando é a unidade natural de atomicidade que o usuário espera ("ou insere tudo ou nada");
* transações longas não devem ser derrubadas por um erro recuperável (chave duplicada).

## Opções consideradas

**Mecanismo de undo**: (a) log de undo em disco; (b) **log de undo em memória**, aplicado ao contrário.

**Semântica de erro numa transação explícita**: (a) qualquer erro aborta tudo (PostgreSQL);
(b) **só o comando falho é desfeito** e a transação segue (estilo MySQL / ponto de salvamento implícito).

## Resultado da decisão

* **(b) undo em memória.** `Txn::undo` guarda ações inversas (`Undo::HeapInsert`, `HeapDelete`,
  `IndexInsert`, `IndexDelete`) na ordem de execução; `Txn::rollback_to(mark)` desfaz de trás para
  frente. O `HeapDelete` guarda o conteúdo apagado e o `RowId`; o undo o recoloca **no mesmo slot**
  (`HeapFile::put_at`).
* **(b) atomicidade por comando.** Antes de cada comando, `Session` guarda um `Mark` (tamanhos de
  `undo` e `wal`). Se o comando falha:
  * em **autocommit**: rollback total (a transação era só aquele comando);
  * em **transação explícita**: `rollback_to(mark)` e a transação **continua aberta**;
  * **exceção**: `Deadlock` e `LockTimeout` abortam a transação **inteira**, porque é preciso soltar
    os locks para destravar os outros.

### Consequências

* **Bom**: rollback rápido e sem E/S; vale para erro, `ROLLBACK`, deadlock e `Drop` de uma sessão.
* **Bom**: `UPDATE` que falha no meio (chave duplicada na fase de inserção) não deixa linhas apagadas.
* **Bom**: o código do executor é escrito de forma "otimista": propaga `?` e confia no `Mark`.
* **Ruim**: o undo precisa caber na memória junto com o WAL pendente (reforça o limite D2).
* **Ruim**: se o próprio undo falhar (disco, pool), o estado fica inconsistente; hoje o erro é
  devolvido ao chamador (`Txn::rollback_to` tenta aplicar todos e devolve o primeiro erro).
* **Ruim**: a semântica difere do PostgreSQL, o que pode surpreender quem vem de lá.

## Validação

| Teste | Garante |
|---|---|
| `txn::rollback_and_commit` | `ROLLBACK` desfaz `INSERT`, `UPDATE` e `DELETE` (incluindo o índice); a chave volta a ficar livre |
| `txn::a_failing_statement_does_not_abort_the_transaction` | erro num comando mantém a transação; o resto é commitado |
| `txn::dropping_a_session_rolls_back` | descartar a sessão com transação aberta faz rollback e solta os locks |
| `sql::unique_index_and_statement_atomicity` | `INSERT (10,'x'),(11,'a')` com a 2ª linha duplicada não deixa a 1ª |
| `recovery::a_transaction_larger_than_the_pool_fails_cleanly_and_the_database_keeps_working` | falha por pool cheio desfaz tudo e o banco segue usável |

## Links

* [`modules/txn.md`](../modules/txn.md),
  [`architecture/runtime-scenarios.md`](../architecture/runtime-scenarios.md#3-rollback)
