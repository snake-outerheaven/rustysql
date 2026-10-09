# ADR-0009: Locks de tabela com *strict two-phase locking*

* **Status**: aceita (com limitação D1)
* **Data**: 2026-10-09
* **Módulos afetados**: `lockmgr`, `db`, `txn`

## Contexto e declaração do problema

Várias transações rodam ao mesmo tempo. Sem controle, uma pode ler dados que outra ainda não
commitou (*dirty read*), ou duas podem atualizar a mesma linha e uma perder a escrita da outra
(*lost update*). Precisamos de uma regra de isolamento, e ela interage com todo o resto: com o rollback
(que desfaz mudanças *in-place* nas páginas) e com a recuperação (que assume que cada tabela é alterada
por uma transação por vez no log).

## Fatores de decisão

* corretude de isolamento acima de desempenho;
* **rollback em memória e WAL por commit** ([ADR-0002](0002-no-steal-no-force-wal-redo.md),
  [ADR-0003](0003-commit-atomico-um-registro.md)): se duas transações escrevessem a mesma página ao
  mesmo tempo, o WAL de cada uma conteria a escrita da outra e o undo ficaria incorreto;
* implementação pequena e verificável.

## Opções consideradas

1. **Lock global do banco** (um escritor por vez).
2. **Locks de tabela, *strict 2PL***, modos compartilhado (S) e exclusivo (X).
3. **Locks de linha** (e de intenção) com 2PL.
4. **MVCC** (leituras de *snapshot* sem bloqueio).

## Resultado da decisão

Escolhida a **opção 2**. `SELECT` pega `Shared` em cada tabela do `FROM`; `INSERT`/`UPDATE`/`DELETE` e
DDL pegam `Exclusive`. Os locks só são soltos no **commit ou rollback** (*strict* 2PL), o que evita
leituras sujas e garante isolamento **serializável por tabela**. Pegam-se as tabelas de um comando em
ordem de id para reduzir deadlocks. Um lock `Shared` pode ser promovido a `Exclusive` (*upgrade*).

### Consequências

* **Bom**: por construção, duas transações nunca escrevem na mesma tabela ao mesmo tempo, então as
  páginas de uma tabela têm **um escritor por vez**: o WAL por transação e o undo em memória são
  corretos sem coordenação adicional.
* **Bom**: isolamento forte e simples de explicar; leitores simultâneos não se bloqueiam.
* **Ruim (D1)**: leitor e escritor da **mesma tabela** se excluem; um escritor lento atrasa todos os
  leitores dessa tabela. Escritores de tabelas diferentes rodam em paralelo.
* **Ruim**: deadlocks são possíveis (ordens diferentes de tabelas entre comandos ou transações) e precisam
  de detecção ([ADR-0010](0010-vitima-de-deadlock.md)).
* **Ruim**: um *upgrade* S→X entre duas transações que seguram S na mesma tabela é um deadlock
  garantido (uma será a vítima).

## Prós e contras das opções

### 1. Lock global
* Prós: nenhum deadlock. Contras: nenhuma concorrência de escrita entre tabelas; tornaria o
  gerenciador de prioridade e o *latch crabbing* menos relevantes.

### 3. Locks de linha
* Prós: muito mais concorrência. Contras: exigiriam locks de **chave** para unicidade, tratamento de
  *phantoms*, e quebrariam a premissa de "um escritor por página" (WAL e undo por transação precisariam
  ser reprojetados: ARIES).

### 4. MVCC
* Contras: versões de linha, coleta de lixo (*vacuum*), regras de visibilidade: um subsistema inteiro.

## Validação

| Propriedade | Teste |
|---|---|
| sem leitura suja; leitor espera o escritor | `txn::readers_wait_for_writers_and_never_see_dirty_data` |
| leitores compartilham | `txn::readers_share_the_lock` |
| nenhum estado parcial visível; dinheiro conservado | `txn::concurrent_transfers_conserve_money_and_never_expose_partial_state` |
| unicidade da PK entre threads concorrentes | `txn::parallel_inserts_into_same_and_different_tables` |

## Links

* [`modules/lockmgr.md`](../modules/lockmgr.md), [`guides/concurrency-and-transactions.md`](../guides/concurrency-and-transactions.md)
