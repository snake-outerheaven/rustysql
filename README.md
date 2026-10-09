# rustysql

Um banco de dados SQL pequeno, escrito em Rust para **aprender como um banco funciona por dentro**.
Sem dependências além do `parking_lot` (locks com guards "owned", necessários para o latch crabbing).

```bash
cargo run --release -- meu.db            # REPL
cargo run --release -- meu.db demo.sql   # executa um script, mostrando tempos
cargo test                               # 45 testes (inclui estresse de concorrência e recuperação)
```

## Camadas

```
SQL ─ lexer ─ parser ─ ast ─ plan (otimizador) ─ exec (operadores) ─┐
                                                                     │
Session / Database (db.rs) ── sched ── lockmgr ── txn ── catalog ────┤
                                                                     │
heap (slotted pages)   btree (latch crabbing)   keyenc ──────────────┤
wal (redo)   bufferpool (latches, no-steal)   disk ──────────────────┘
```

| módulo | o que faz |
|---|---|
| `disk` | páginas de 4 KB num arquivo |
| `bufferpool` | cache de páginas; cada página é um `Arc<Frame>` (o *pin*) com um `RwLock` (o *latch*) |
| `heap` | registros em *slotted pages*; compactação; `RowId` estável |
| `btree` | B+tree em disco, thread-safe (**latch crabbing**); chaves em bytes (`keyenc`) |
| `wal` | log de **redo**; um registro atômico (com CRC) por transação commitada |
| `lockmgr` | locks de tabela S/X (**strict 2PL**), detecção de deadlock, preferência a escritores |
| `sched` | **gerenciador de prioridade**: controle de admissão com `LOW/NORMAL/HIGH` e aging |
| `plan`/`exec` | escolha de índice, pushdown, hash/index/nested-loop join, agregação (modelo Volcano) |
| `db` | `Database` (handle `Clone + Send + Sync`) e `Session` (conexão) |

## SQL

`CREATE TABLE`, `CREATE [UNIQUE] INDEX`, `DROP TABLE|INDEX`, `INSERT` (várias linhas), `UPDATE`, `DELETE`,
`SELECT` com `JOIN`/`LEFT JOIN`, `WHERE` (`AND OR NOT IS NULL BETWEEN IN LIKE`, aritmética com `+ - * / %`),
`GROUP BY`, `HAVING`, `COUNT SUM AVG MIN MAX`, `ORDER BY` (alias/posição), `LIMIT/OFFSET`,
`BEGIN/COMMIT/ROLLBACK`, `CHECKPOINT`, `EXPLAIN`, `SET PRIORITY LOW|NORMAL|HIGH`.

## Concorrência: três camadas, três perguntas

| pergunta | mecanismo |
|---|---|
| **quem entra?** | `sched`: no máximo N comandos rodando; fila ordenada por prioridade com *aging* |
| **quem pode tocar nos dados?** | `lockmgr`: 2PL por tabela, isolamento serializável por tabela |
| **a estrutura continua íntegra?** | `bufferpool` + `btree`: latches de página, travando sempre de cima para baixo |

Detalhes que valem ler no código:

* **Latch crabbing** (`btree.rs`): a busca pega o latch do filho antes de soltar o do pai. A inserção tenta
  o caminho otimista (latch exclusivo só na folha) e, se a folha vai dividir, desce com latches exclusivos
  soltando os ancestrais assim que o filho é *seguro* (não divide). Os latches ficam em guards *owned*
  (`Arc<RwLock<..>>`), o que permite segurar o pai e o filho ao mesmo tempo.
* **Aging** (`sched.rs`): a chave de cada espera é `nível × passo − instante_de_chegada`; a ordem da fila nunca
  muda depois de montada, mas quem espera o bastante ultrapassa os mais prioritários recém-chegados.
* **Vaga cedida ao bloquear**: um comando esperando lock devolve a vaga e a retoma com prioridade máxima. Sem
  isso, leitores bloqueados por um escritor ocupariam todas as vagas e o escritor não conseguiria terminar.
* **Vítima de deadlock** (`lockmgr.rs`): menor prioridade, depois menos locks exclusivos, depois o mais novo.
  (Abortar sempre quem fecha o ciclo esfomeava os escritores: foi um bug real, achado pelo teste de transferências.)

## Durabilidade

*no-steal / no-force*: páginas sujas só vão ao disco num **checkpoint** (que espera as transações acabarem),
então o arquivo de dados só contém estados commitados e o rollback é só desfazer em memória. O commit grava o
WAL (`fsync`); a abertura refaz o WAL (operações idempotentes) e reconstrói os índices, que não são logados.
Um registro de WAL cortado ou corrompido no fim é descartado pelo CRC.

## Limites conhecidos

* locks só por tabela (não há locks de linha nem MVCC);
* uma transação precisa caber no buffer pool (*no-steal*): senão falha com "buffer pool cheio";
* `delete` da B-tree não funde nós; `DROP TABLE` não recicla páginas;
* chaves de índice com no máximo 512 bytes; uma só coluna por índice; só uma `PRIMARY KEY`;
* sem `SELECT` sem `FROM`, subconsultas, `DISTINCT`, `UNION` nem `ALTER TABLE`.
