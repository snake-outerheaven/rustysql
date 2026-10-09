# ADR-0002: *No-steal / no-force*, WAL de redo e checkpoint *sharp*

* **Status**: aceita
* **Data**: 2026-10-09
* **Módulos afetados**: `bufferpool`, `wal`, `db`, `txn`

## Contexto e declaração do problema

O banco mantém páginas em memória (buffer pool) e precisa garantir duas coisas ao mesmo tempo:

* **Durabilidade**: o que foi commitado sobrevive a uma queda;
* **Atomicidade**: o que **não** foi commitado não deixa rastros depois da queda.

A tensão está em *quando* as páginas vão ao disco. A literatura (Gray/Reuter, ARIES) classifica as
políticas por duas perguntas:

| | **Steal** (pode gravar página suja de transação não commitada) | **No-steal** |
|---|---|---|
| **Force** (grava as páginas no commit) | exige undo + sem redo | sem undo, sem redo |
| **No-force** (não grava no commit) | exige **undo e redo** (ARIES) | exige só **redo** |

## Fatores de decisão

* simplicidade da recuperação (tudo que for desnecessário é um bug a menos);
* desempenho do commit: gravar um log sequencial é muito mais barato que gravar páginas espalhadas;
* o rollback precisa existir de qualquer forma (a transação pode desistir com o processo vivo).

## Opções consideradas

1. **Steal + no-force (ARIES completo)**: log de undo e redo físico/fisiológico, LSN por página, análise/redo/undo.
2. **Force + no-steal**: gravar todas as páginas modificadas a cada commit.
3. **No-steal + no-force + checkpoint *sharp***: páginas sujas **só** vão ao disco num checkpoint
   que espera todas as transações; o log contém só **redo**.

## Resultado da decisão

Escolhida a **opção 3**.

* `BufferPool` nunca despeja nem grava uma página suja (`make_room` só escolhe páginas limpas e sem pin).
* O **commit** grava um registro de redo no WAL e faz `fsync`; não toca no arquivo de dados.
* O **checkpoint** (`Inner::checkpoint`) pega o portão em modo exclusivo (espera as transações ativas e
  bloqueia as novas), grava todas as páginas sujas, faz `fsync` e só então esvazia o WAL.
* Como o arquivo de dados só contém estados commitados, **não existe undo em disco**: o rollback é
  feito em memória pelo log de undo da transação ([ADR-0013](0013-rollback-em-memoria-e-atomicidade-por-comando.md)).

### Consequências

* **Bom**: a recuperação é "reaplicar o WAL"; não há fase de análise nem de undo.
* **Bom**: o commit é uma escrita sequencial e um `fsync`.
* **Bom**: se uma queda atingir uma transação aberta, nada dela está no arquivo nem no log.
* **Ruim (limite de tamanho)**: a transação inteira tem que caber no buffer pool, porque nenhuma
  página suja pode sair. Um `INSERT` gigante falha com "buffer pool cheio" (dívida D2).
* **Ruim (pausa)**: o checkpoint é uma pausa: espera as transações ativas e bloqueia as novas.
  Uma transação explícita esquecida aberta bloqueia o checkpoint (dívida D6).
* **Ruim (torn page)**: se a queda ocorrer *durante* a gravação de uma página no checkpoint, e o
  sistema de arquivos não garantir escrita atômica de 4 KB, a página pode ficar pela metade
  (risco R1). O redo idempotente cobre a maior parte dos casos; um *double-write buffer* resolveria.

## Prós e contras das opções

### 1. ARIES completo
* Prós: transações maiores que a memória; checkpoints "fuzzy" sem pausa; é o estado da arte.
* Contras: undo/redo físico, LSN em cada página, tabela de páginas sujas, CLRs (registros de compensação):
  várias vezes mais código e superfície de bugs, fora do escopo de um projeto de estudo.

### 2. Force + no-steal
* Prós: sem redo. Contras: cada commit grava várias páginas aleatórias; e continua precisando de um
  mecanismo para tornar **atômica** a gravação de várias páginas (um log!), ou seja, voltaria à opção 3.

### 3. No-steal + no-force
* Prós/contras: acima.

## Validação

| Propriedade | Teste |
|---|---|
| commit sobrevive à queda | `recovery::committed_work_survives_a_crash_including_indexes` |
| transação aberta some | `recovery::an_uncommitted_transaction_is_lost_on_crash` |
| checkpoint esvazia o WAL e o redo posterior funciona | `recovery::checkpoint_empties_the_wal_and_later_commits_replay_on_top_of_it` |
| reaplicar o log é inofensivo | `recovery::replaying_the_log_twice_gives_the_same_result` |
| pool pequeno: páginas limpas são despejadas, checkpoint automático | `recovery::small_buffer_pool_evicts_clean_pages_and_checkpoints_automatically` |
| transação maior que o pool falha limpo | `recovery::a_transaction_larger_than_the_pool_fails_cleanly_and_the_database_keeps_working` |

## Links

* Gray & Reuter, *Transaction Processing: Concepts and Techniques* (1993), cap. 9.
* [`modules/wal.md`](../modules/wal.md), [`modules/bufferpool.md`](../modules/bufferpool.md),
  [`architecture/runtime-scenarios.md`](../architecture/runtime-scenarios.md#5-checkpoint)
