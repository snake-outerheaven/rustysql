# Estratégia de testes

Como o rustysql é testado, o que cada teste garante, e como acrescentar um novo sem enfraquecer a
suíte. **O número é 1 unitário + 44 de integração + 7 exemplos de rustdoc = 52 testes.**

## Princípios

1. **Teste a propriedade, não a implementação.** "Dinheiro nunca some" (`concurrent_transfers_*`) vale
   independentemente de como os locks são implementados.
2. **Todo bug achado vira um teste** (e o teste é validado por mutação; veja abaixo).
3. **Concorrência se testa com invariantes**, não com tempo: nada de `sleep` para "esperar a corrida".
   Onde a ordem importa, o teste a **força** (esperando um estado observável, como "o leitor está
   bloqueado").
4. **Cada camada é testável sozinha.** A B-tree tem testes sem SQL; o escalonador, sem banco.
5. **Falha determinística > sorte.** Os testes de estresse são repetidos dezenas de vezes antes de
   confiar neles.

## Como rodar

```bash
cargo test                           # tudo (poucos segundos depois de compilado)
cargo test --test btree              # um arquivo de testes
cargo test concurrent_transfers      # por nome
cargo test --release                 # estresse: bem mais rápido, escalonamento diferente
cargo test --doc                     # só os exemplos do rustdoc
cargo test -- --nocapture            # mostra println! (ex.: nº de deadlocks resolvidos)
cargo clippy --all-targets           # deve ficar sem avisos
cargo doc --no-deps                  # deve ficar sem avisos (links quebrados falham o lint)
```

Os testes usam arquivos temporários (`std::env::temp_dir()`) com o PID no nome; cada teste apaga o seu
`.db` e `.wal` antes de começar.

## Mapa: arquivo → camada → o que garante

| Arquivo | Testes | Camada | Foco |
|---|---:|---|---|
| `src/keyenc.rs` (unitário) | 1 | codificação de chaves | ordem preservada |
| `tests/basic.rs` | 3 | ponta a ponta | CRUD, restrições, persistência |
| `tests/btree.rs` | 5 | B-tree isolada | invariantes e **concorrência** de latches |
| `tests/sql.rs` | 7 | parser, planejador, executor | índices, junções, agregações, expressões |
| `tests/txn.rs` | 8 | transações e locks | rollback, isolamento, deadlock, estresse |
| `tests/sched.rs` | 10 | escalonador e integração | prioridade, aging, vaga, vítima de deadlock |
| `tests/recovery.rs` | 11 | durabilidade | WAL, quedas, checkpoint, pool pequeno |
| rustdoc (`///` e `//!`) | 7 | API pública | os exemplos **compilam e rodam** |

## Catálogo completo

### `tests/basic.rs`: ponta a ponta

| Teste | Propriedade |
|---|---|
| `crud_flow` | `INSERT`/`SELECT`/`UPDATE`/`DELETE`, `ORDER BY`, `LIMIT`, `IS NULL`, lógica de três valores |
| `constraints` | chave duplicada, `NOT NULL`, tipo errado e tabela inexistente são recusados e não deixam rastro |
| `persistence_and_many_pages` | 500 linhas em várias páginas sobrevivem ao fechar e reabrir; reuso de slots depois de apagar |

### `tests/btree.rs`: a estrutura e a concorrência de latches

| Teste | Propriedade |
|---|---|
| `random_inserts_search_delete_keep_invariants` | 6000 chaves embaralhadas: `verify()` em passos, busca de todas, duplicata recusada, remoção da metade |
| `range_scan_is_ordered_and_bounded` | limites inclusivo/exclusivo, atravessa folhas, início entre chaves existentes |
| `long_text_keys_force_deep_tree` | chaves de ~300 B geram ≥ 3 níveis; chave de 600 B e chave vazia são recusadas |
| `concurrent_writers_and_readers_preserve_integrity` | 8 escritores com faixas intercaladas + 4 leitores (busca e varredura): **chaves âncora nunca somem**, varredura sempre ordenada, `verify()` no fim |
| `concurrent_inserts_and_deletes_with_deep_tree` | 6 threads inserindo e apagando chaves de 150 B numa árvore de ≥ 3 níveis |

### `tests/sql.rs`: SQL

| Teste | Propriedade |
|---|---|
| `secondary_index_is_chosen_and_stays_consistent` | índice escolhido por igualdade e intervalos; **mesmos resultados com e sem índice**; acompanha `UPDATE`/`DELETE`; `DROP INDEX` |
| `primary_key_uses_the_index_even_inside_and` | índice dentro de `AND`, com o filtro completo reaplicado; `UPDATE`/`DELETE` planejam por índice |
| `unique_index_and_statement_atomicity` | índice único, vários `NULL` permitidos; comando que falha **não deixa linhas**; `CREATE UNIQUE INDEX` falho não deixa o índice |
| `joins_choose_the_right_algorithm_and_compute_correctly` | `IndexNestedLoopJoin`, `HashJoin`, `NestedLoopJoin`, `LEFT JOIN`, pushdown, coluna ambígua, alias repetido |
| `aggregates_group_by_having_order_limit` | `COUNT/SUM/AVG/MIN/MAX`, agregação global vazia, `GROUP BY`, `HAVING`, `ORDER BY` por alias/posição, `LIMIT/OFFSET` |
| `expressions_like_between_in` | `LIKE`, `BETWEEN`, `IN`, `IS NULL`, `NULL` em comparações, `%`, aliases |
| `update_changing_primary_keys_in_bulk_and_ddl_errors` | `SET id = id + 1000` em 300 linhas sem falsa colisão; colisão real é erro e não muda nada; erros de DDL |

### `tests/txn.rs`: transações, isolamento e estresse

| Teste | Propriedade |
|---|---|
| `rollback_and_commit` | `ROLLBACK` desfaz `INSERT`/`UPDATE`/`DELETE` (índice incluído); `COMMIT` sem `BEGIN` e `BEGIN` aninhado são erros |
| `a_failing_statement_does_not_abort_the_transaction` | erro num comando mantém a transação; DDL em transação explícita é recusado |
| `dropping_a_session_rolls_back` | descartar a `Session` desfaz a transação e solta os locks |
| `readers_wait_for_writers_and_never_see_dirty_data` | o leitor **espera** o escritor (não termina em 300 ms) e nunca vê dado não commitado |
| `readers_share_the_lock` | dois leitores não se bloqueiam |
| `deadlock_is_detected_and_one_victim_is_chosen` | duas threads em ordem oposta: **exatamente uma** vítima; a outra aplica as duas mudanças |
| `concurrent_transfers_conserve_money_and_never_expose_partial_state` | 6 transferências concorrentes com ordens de lock opostas + 2 auditores: **a soma é sempre 2000** |
| `parallel_inserts_into_same_and_different_tables` | 600 inserções paralelas na mesma tabela e 4 tabelas próprias; **só uma** de 4 threads vence a disputa por uma chave primária |

### `tests/sched.rs`: gerenciador de prioridade

| Teste | Propriedade |
|---|---|
| `higher_priority_enters_first_and_fifo_within_a_level` | ordem `high1, high2, normal1, normal2, low` |
| `transaction_holders_and_resumed_statements_outrank_user_priorities` | `resume > in-txn > high > normal` |
| `aging_lets_a_long_waiting_low_overtake_a_fresh_high` | anti-inanição; e o controle: sem espera suficiente a prioridade manda |
| `never_runs_more_than_max_running` | 12 threads × 15 rodadas: o pico é **exatamente** o limite, nunca mais; contadores por nível |
| `queue_timeout_gives_up_and_does_not_block_those_behind` | `QueueTimeout` e a desistência não trava a fila |
| `yielding_a_permit_frees_the_slot_and_resume_waits_for_one` | ceder e retomar a vaga |
| `a_statement_blocked_on_a_lock_gives_its_slot_back` | com **uma** vaga, leitor bloqueado + escritor terminam (sem deadlock de admissão) |
| `set_priority_changes_how_statements_are_admitted` | `SET PRIORITY` e estatísticas, incluindo o contador "em transação" |
| `deadlock_victim_is_the_lowest_priority_not_the_last_to_ask` | a vítima é a `LOW`, **nas duas ordens de chegada** |
| `with_equal_priority_the_victim_is_the_one_with_less_work_to_lose` | desempate por locks exclusivos |

### `tests/recovery.rs`: durabilidade

| Teste | Propriedade |
|---|---|
| `committed_work_survives_a_crash_including_indexes` | commits sobrevivem; os índices voltam e **são usados** (`EXPLAIN`); a PK continua valendo; o WAL esvazia |
| `an_uncommitted_transaction_is_lost_on_crash` | transação aberta some por inteiro (queda simulada com `mem::forget`) |
| `a_torn_wal_tail_drops_only_the_unfinished_commit` | WAL cortado ao meio: só o último commit se perde |
| `a_corrupted_wal_record_is_detected_by_the_checksum` | um byte invertido: o CRC pega |
| `checkpoint_empties_the_wal_and_later_commits_replay_on_top_of_it` | checkpoint + redo posterior |
| `replaying_the_log_twice_gives_the_same_result` | **idempotência** do redo (60 linhas, deletes e updates) |
| `pages_allocated_by_a_rolled_back_transaction_still_work_after_recovery` | operações estruturais sobrevivem ao rollback |
| `ddl_survives_a_crash` | `CREATE`/`DROP` de tabelas e índices sobrevivem |
| `a_clean_shutdown_checkpoints_and_leaves_an_empty_wal` | `Drop` faz checkpoint (WAL com 0 bytes) |
| `small_buffer_pool_evicts_clean_pages_and_checkpoints_automatically` | 3000 linhas num pool de 48 páginas: despejo e checkpoint automáticos |
| `a_transaction_larger_than_the_pool_fails_cleanly_and_the_database_keeps_working` | pool de 32 páginas: falha com "buffer pool", **desfaz tudo**, o banco segue utilizável |

## Técnicas usadas

### Simular uma queda

| Quero simular | Como |
|---|---|
| queda **depois** de commits | `db.simulate_crash(); drop(db);` (o `Drop` não faz o checkpoint final) |
| queda **no meio** de uma transação | `std::mem::forget(session); std::mem::forget(db);` (nada é liberado nem desfeito) |
| WAL cortado | `OpenOptions::new().write(true).open(wal).set_len(n)` |
| WAL corrompido | abrir o arquivo e inverter um byte |
| WAL "que voltou" (queda no checkpoint) | copiar o `.wal` antes de fechar e restaurá-lo depois |

### Forçar uma ordem de eventos sem `sleep`

Os testes de deadlock e de bloqueio **esperam um estado observável** em vez de dormir:

```rust
wait_until("A bloqueado", || db.lock_snapshot().matches("espera por").count() == 1);
```

e os de fila esperam `scheduler.stats().queued == n` antes de lançar o próximo comando. O raro
`recv_timeout(300 ms)` usado para provar que "o leitor **não** terminou" é o único tempo fixo, e é
unilateral (um sistema lento só o deixa mais seguro).

### Teste de mutação (manual)

Um teste de concorrência que **passa** não prova nada se ele também passaria com o bug. A prática:
**quebrar o código de propósito** e ver o teste falhar.

| Mutação | Teste que deve falhar | Resultado |
|---|---|---|
| soltar o latch do pai **antes** de pegar o do filho (`find_leaf_read`) | `btree::concurrent_writers_and_readers_preserve_integrity` | a **1ª versão do teste sobreviveu** (os leitores buscavam chaves num canto que nenhuma divisão tocava); reforçado com chaves âncora intercaladas, passou a **falhar** com a mutação |
| `pick_victim` devolve o requisitante | os dois testes de vítima em `sched` | falham |
| `rollback_to` descarta também as operações estruturais | `recovery::pages_allocated_by_a_rolled_back_transaction_*` | falha: só **18 das 400** linhas commitadas sobrevivem |

Procedimento: edite, rode o teste esperando falha, **reverta** (cuidado: restaurar uma cópia mantém o
*timestamp* antigo e o Cargo pode não recompilar: use `touch`).

### Caça a instabilidade (*flaky tests*)

Antes de confiar num teste concorrente, rode-o em laço, em `--release` (outro escalonamento):

```bash
for i in $(seq 1 25); do cargo test --release --test btree -q || break; done
```

A suíte concorrente (`btree`, `txn`, `sched`, `recovery`) foi executada **20 vezes seguidas (680
execuções)** sem falha nem travamento, cada rodada com limite de tempo para detectar *hangs*.

## Regressões que viraram testes

| Problema encontrado | Como apareceu | Teste |
|---|---|---|
| vítima de deadlock sempre o escritor → inanição | o teste de transferências travou >20 s | `sched::deadlock_victim_*`, `txn::concurrent_transfers_*` ([ADR-0010](../adr/0010-vitima-de-deadlock.md)) |
| fluxo de leitores esfomeia o escritor | idem | `txn::concurrent_transfers_*` ([ADR-0011](../adr/0011-preferencia-a-escritores.md)) |
| páginas alocadas por uma transação abortada somem do log | análise do desenho | `recovery::pages_allocated_*` ([ADR-0005](../adr/0005-operacoes-estruturais-sobrevivem-ao-rollback.md)) |
| `%` não existia (o teste usava) | falha de parse num teste | `sql::expressions_like_between_in` |
| arestas obsoletas no grafo de espera | revisão do gerenciador de locks | coberto indiretamente pelo estresse |

## Como acrescentar um teste

1. **Escolha a camada mais baixa** que exprime a propriedade (B-tree direta? `Scheduler` direto? SQL?).
2. Use um arquivo temporário **único** (`format!("..._{name}_{}.db", std::process::id())`) e remova-o antes.
3. Desligue o `fsync` (`Options { sync_commits: false, .. }`) a menos que durabilidade seja o ponto.
4. Para concorrência: **invariantes** e *barriers*/estados observáveis; evite `sleep`.
5. **Prove que o teste pode falhar** (mutação) antes de confiar nele.
6. Se corrige um bug, cite-o no comentário do teste e acrescente à tabela de regressões acima.

## Lacunas conhecidas

* Não há testes de **desempenho** automatizados (os números do `demo.sql` são manuais).
* Não há *fuzzing* do parser nem testes baseados em propriedades (`proptest`).
* Falhas de **disco** (disco cheio, `fsync` com erro) não são injetadas.
* A recuperação de uma *torn page* (4 KB gravados pela metade) não é testada (risco R1 do arc42).
* Sem verificação de memória/UB (o projeto não tem `unsafe`; mesmo assim, `miri` e *sanitizers* não rodam em CI).
