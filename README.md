# rustysql

**Um banco de dados relacional embutido, escrito em Rust para você _ler, rodar e quebrar_.**
B+tree concorrente, WAL, transações ACID, SQL com otimizador e um gerenciador de prioridade de queries:
cada peça de um banco de verdade, pequena o bastante para ser lida inteira, **sem uma linha de `unsafe`**.

[![CI](https://github.com/snake-outerheaven/rustysql/actions/workflows/ci.yml/badge.svg)](https://github.com/snake-outerheaven/rustysql/actions/workflows/ci.yml)
[![Licença: MIT](https://img.shields.io/badge/licen%C3%A7a-MIT-blue.svg)](LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](Cargo.toml)
[![unsafe: 0](https://img.shields.io/badge/unsafe-0-success.svg)](docs/development/conventions.md#estilo-de-código)
[![Contribuições bem-vindas](https://img.shields.io/badge/PRs-bem--vindos-brightgreen.svg)](CONTRIBUTING.md)

> **Não é um substituto do SQLite.** É um banco para **aprender como bancos funcionam por dentro**. Cada
> decisão tem um [registro (ADR)](docs/adr/README.md) com as alternativas e o que ela custou, e toda a
> documentação foi escrita para quem está lendo o código pela primeira vez.

## Por que olhar este projeto

* **Você consegue ler tudo.** ~6 mil linhas, um arquivo por conceito, comentários que explicam o *porquê*.
* **Concorrência de verdade**, e testada: a B-tree aguenta escritores e leitores simultâneos
  (*latch crabbing*), e os testes foram validados **quebrando o código de propósito** (teste de mutação).
* **Durabilidade de verdade**: mate o processo no meio de uma transação, ou corte o log no meio de um
  commit, e o banco volta consistente.
* **Documentação de nível profissional**: arquitetura (arc42), diagramas C4, 15 ADRs, referência por módulo
  e os **bytes reais** do arquivo em disco.
* **Honesto sobre os limites**: o que não faz está listado, com o motivo.

## Veja funcionando

```text
$ cargo run --release -- loja.db
rustysql> CREATE TABLE clientes (id INT PRIMARY KEY, nome TEXT NOT NULL, cidade TEXT);
rustysql> INSERT INTO clientes VALUES (1,'Ana','Recife'),(2,'Beto','Salvador'),(3,'Carla','Recife'),(4,'Davi',NULL);
rustysql> SELECT * FROM clientes WHERE cidade = 'Recife' ORDER BY nome DESC;
| id | nome  | cidade |
|----|-------|--------|
| 3  | Carla | Recife |
| 1  | Ana   | Recife |
(2 linha(s))
```

O banco **mostra o que faz**. Sem índice, uma busca lê a tabela inteira; com índice, só o trecho necessário:

```text
rustysql> EXPLAIN SELECT * FROM pedidos WHERE cliente_id = 1;
Project [pedidos.id, pedidos.cliente_id, pedidos.valor]
  Filter (cliente_id = 1)
    SeqScan pedidos

rustysql> CREATE INDEX idx_pedido_cliente ON pedidos(cliente_id);
rustysql> EXPLAIN SELECT * FROM pedidos WHERE cliente_id = 1;
Project [pedidos.id, pedidos.cliente_id, pedidos.valor]
  Filter (cliente_id = 1)
    IndexScan pedidos using idx_pedido_cliente (cliente_id = 1)
```

Transações com atomicidade por comando, e prioridades que ordenam **quem entra**:

```text
rustysql*> BEGIN;                       -- o prompt ganha um * dentro de uma transação
rustysql*> UPDATE clientes SET nome = 'Ana Maria' WHERE id = 1;
rustysql*> INSERT INTO clientes VALUES (1, 'duplicado', NULL);   -- erro: só este comando é desfeito
error: chave primária duplicada: 1
rustysql*> COMMIT;
rustysql> SET PRIORITY HIGH;            -- esta sessão passa na frente na fila de admissão
```

*Medido no Windows, modo `--release`, no script [`demo.sql`](demo.sql): inserir 20 mil linhas num único
`INSERT` leva ~0,5 s; buscar por chave primária ~0,2 ms; buscar por uma coluna indexada ~50 µs contra
~6 ms sem o índice. Seus números vão variar.*

## Início rápido

Requer **Rust 1.88+** ([rustup.rs](https://rustup.rs)).

```bash
git clone https://github.com/snake-outerheaven/rustysql.git && cd rustysql
cargo run --release -- meu.db            # terminal interativo
cargo run --release -- meu.db demo.sql   # executa um script, mostrando o tempo de cada comando
cargo test                               # 52 testes (inclui estresse de concorrência e recuperação de quedas)
cargo run --example storage_dump         # mostra os bytes REAIS que o banco grava em disco
cargo doc --no-deps --open               # referência de API (todo item público é documentado)
```

## Como biblioteca

```rust
use rustysql::{Database, QueryResult, Value};

let db = Database::open("loja.db")?;                       // Clone + Send + Sync
db.run("CREATE TABLE emp (id INT PRIMARY KEY, nome TEXT, salario FLOAT)")?;
db.run("INSERT INTO emp VALUES (1, 'ana', 100), (2, 'bia', 250), (3, 'caio', 180)")?;

let mut out = db.run("SELECT nome FROM emp WHERE salario > 150 ORDER BY salario DESC")?;
let Some(QueryResult::Rows { rows, .. }) = out.pop() else { unreachable!() };
assert_eq!(rows, vec![vec![Value::Text("bia".into())], vec![Value::Text("caio".into())]]);
# Ok::<(), rustysql::Error>(())
```

Uma `Session` por thread dá transações isoladas (`BEGIN`/`COMMIT`), com retry em deadlock:
veja o [guia da API](docs/guides/embedding-api.md).

### App desktop de exemplo

[`studio/`](studio/README.md) é um aplicativo **Tauri 2** que usa a biblioteca como qualquer cliente faria: console SQL com transações abertas, navegador de dados, monitor ao vivo (WAL, páginas sujas, fila de prioridade, locks) e um laboratório de transferências concorrentes que confere a integridade no fim. Frontend em HTML/JS puro, sem Node.

```bash
cd studio && cargo run --release
```

## Aprenda lendo (um roteiro)

Uma ordem que ensina bem, de baixo para cima:

| # | Leia | Você aprende |
|--:|---|---|
| 1 | [`disk`](docs/modules/disk.md) → [`bufferpool`](docs/modules/bufferpool.md) | páginas de 4 KB; cache com *latches* e *pin* por `Arc` |
| 2 | [`heap`](docs/modules/heap.md) · [ADR-0001](docs/adr/0001-paginas-4kb-slotted-pages.md) | *slotted pages*; por que o endereço de uma linha é estável |
| 3 | [`keyenc`](docs/modules/keyenc.md) → [`btree`](docs/modules/btree.md) · [ADR-0006](docs/adr/0006-btree-latch-crabbing.md) | chaves comparáveis por `memcmp`; **latch crabbing** |
| 4 | [`wal`](docs/modules/wal.md) → [`txn`](docs/modules/txn.md) · [ADR-0002](docs/adr/0002-no-steal-no-force-wal-redo.md) | *no-steal/no-force*, redo, checkpoint, recuperação |
| 5 | [`lockmgr`](docs/modules/lockmgr.md) → [`sched`](docs/modules/sched.md) · [ADR-0010](docs/adr/0010-vitima-de-deadlock.md) | 2PL, deadlock, **prioridade e aging** |
| 6 | [`lexer`](docs/modules/lexer.md) → [`parser`](docs/modules/parser.md) → [`plan`](docs/modules/plan.md) → [`exec`](docs/modules/exec.md) | do texto SQL ao resultado (Volcano) |

## Histórias de bugs reais

Estas aconteceram **durante o desenvolvimento** e estão registradas, com a medição, nos ADRs:

* **O teste que travou por mais de 20 segundos.** Um leitor "inocente" nunca perdia um deadlock; o escritor
  era a vítima **toda vez** e tentava de novo, para sempre: entre 11 mil e 62 mil *retries*. A correção
  (escolher a vítima por prioridade e custo) levou a **61**. → [ADR-0010](docs/adr/0010-vitima-de-deadlock.md)
* **O teste de concorrência que passava mesmo com o bug.** Quebrar o protocolo de latches de propósito
  *não* o fazia falhar: os leitores olhavam um canto da árvore que nenhuma divisão tocava. O teste foi
  reforçado até **detectar** a mutação. → [ADR-0006](docs/adr/0006-btree-latch-crabbing.md)
* **Linhas commitadas que sumiam depois de uma queda.** Se uma transação abortada alocava páginas, as
  transações seguintes escreviam nelas e o log não sabia que existiam: sem a correção, **só 18 de 400**
  linhas commitadas sobreviviam à recuperação. → [ADR-0005](docs/adr/0005-operacoes-estruturais-sobrevivem-ao-rollback.md)

## O que tem

| Camada | Módulos | O que faz |
|---|---|---|
| Base | `disk`, `bufferpool` | páginas de 4 KB; cache com `Arc<Frame>` (o *pin*) e `RwLock` (o *latch*); *no-steal* |
| Armazenamento | `heap`, `btree`, `keyenc`, `wal` | *slotted pages*; B+tree concorrente; chaves em bytes; log de redo com CRC |
| Concorrência | `lockmgr`, `sched`, `txn` | locks de tabela (*strict 2PL*) com deadlock; **fila de admissão por prioridade**; undo |
| SQL | `lexer`, `parser`, `plan`, `exec`, `eval` | otimizador por regras, junções por índice/hash/laço, agregações, `EXPLAIN` |
| Motor | `db`, `catalog` | `Database` (`Clone + Send + Sync`) e `Session`; recuperação; DDL |

**SQL**: `CREATE TABLE`, `CREATE [UNIQUE] INDEX`, `DROP`, `INSERT` (várias linhas), `UPDATE`, `DELETE`,
`SELECT` com `JOIN`/`LEFT JOIN`, `WHERE` (`AND OR NOT IS NULL BETWEEN IN LIKE`, `+ - * / %`), `GROUP BY`,
`HAVING`, `COUNT SUM AVG MIN MAX`, `ORDER BY`, `LIMIT/OFFSET`, `BEGIN/COMMIT/ROLLBACK`, `CHECKPOINT`,
`EXPLAIN`, `SET PRIORITY`. Referência completa: [`docs/reference/sql.md`](docs/reference/sql.md).

### Concorrência: três camadas, três perguntas

| Pergunta | Mecanismo |
|---|---|
| **Quem entra?** | `sched`: no máximo N comandos rodando; fila ordenada por prioridade com *aging* |
| **Quem pode tocar nos dados?** | `lockmgr`: 2PL por tabela, isolamento serializável por tabela |
| **A estrutura continua íntegra?** | `bufferpool` + `btree`: latches de página, sempre de cima para baixo |

### Durabilidade

*No-steal / no-force*: páginas sujas só vão ao disco num **checkpoint**, então o arquivo de dados só contém
estados commitados e o rollback é só desfazer em memória. O commit grava o WAL (`fsync`); a abertura refaz o
WAL (operações idempotentes) e reconstrói os índices. Um registro cortado ou corrompido no fim do log é
descartado pelo CRC.

## Documentação

Tudo está em [`docs/`](docs/README.md), com um modelo profissional para cada finalidade:

| Quero... | Leia |
|---|---|
| **começar** | [Primeiros passos](docs/guides/getting-started.md) · [Referência da CLI](docs/reference/cli.md) |
| **usar o SQL** | [Referência de SQL](docs/reference/sql.md) · [Ajuste de consultas (`EXPLAIN`)](docs/guides/query-tuning.md) |
| **embutir em Rust** | [Guia da API](docs/guides/embedding-api.md) · [Transações e concorrência](docs/guides/concurrency-and-transactions.md) |
| **entender a arquitetura** | [arc42](docs/architecture/arc42.md) · [diagramas C4](docs/architecture/c4.md) · [cenários de execução](docs/architecture/runtime-scenarios.md) |
| **saber por que é assim** | [15 decisões (ADRs)](docs/adr/README.md) |
| **estudar um módulo** | [referência por módulo](docs/modules/README.md) |
| **operar / diagnosticar** | [Runbook](docs/guides/operations-runbook.md) · [Erros](docs/reference/errors.md) · [Configuração](docs/reference/configuration.md) |
| **ver os bytes em disco** | [Formatos de armazenamento](docs/reference/storage-formats.md) |
| **contribuir** | [CONTRIBUTING](CONTRIBUTING.md) · [Testes](docs/development/testing.md) · [Convenções](docs/development/conventions.md) · [CHANGELOG](CHANGELOG.md) |

## Limites conhecidos

* locks só por tabela (não há locks de linha nem MVCC);
* uma transação precisa caber no buffer pool (*no-steal*): senão falha com "buffer pool cheio";
* `delete` da B-tree não funde nós; `DROP TABLE` não recicla páginas;
* chaves de índice com no máximo 512 bytes; uma só coluna por índice; só uma `PRIMARY KEY`;
* sem `SELECT` sem `FROM`, subconsultas, `DISTINCT`, `UNION` nem `ALTER TABLE`;
* **não foi auditado**: não guarde dados que importam sem avaliar os riscos por conta própria.

A lista completa, com o plano de cada dívida, está na
[seção 11 do arc42](docs/architecture/arc42.md#11-riscos-e-dívida-técnica).

## Contribuindo

Contribuições são muito bem-vindas, **inclusive de quem está aprendendo**. Há
[issues para começar](https://github.com/snake-outerheaven/rustysql/labels/good%20first%20issue) e
[tarefas maiores](https://github.com/snake-outerheaven/rustysql/labels/help%20wanted), cada uma com os
arquivos a mexer e os critérios de aceite. Dúvidas e ideias:
[Discussions](https://github.com/snake-outerheaven/rustysql/discussions). Leia o
[CONTRIBUTING](CONTRIBUTING.md) e o [código de conduta](CODE_OF_CONDUCT.md).

Se o projeto te ajudou a entender algo, uma ⭐ ajuda outras pessoas a encontrá-lo.

## Licença

[MIT](LICENSE): uso, cópia, modificação e distribuição livres, inclusive **comerciais**, desde que o aviso de
copyright seja mantido. O software é fornecido **sem garantia**.
