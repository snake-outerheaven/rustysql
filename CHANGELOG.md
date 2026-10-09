# Changelog

Todas as mudanças relevantes deste projeto são registradas aqui.

O formato segue o [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/). O projeto ainda não
tem versões publicadas: a numeração segue o `Cargo.toml` (`0.1.0`) e **o formato de arquivo muda sem
migração** (veja o *magic* em [`storage-formats.md`](docs/reference/storage-formats.md#compatibilidade-e-versão)).

## [Não lançado]

### Adicionado
- **`studio/`**: aplicativo desktop de exemplo (Tauri 2) para gerenciar um banco: console SQL, navegador
  de dados, monitor ao vivo e laboratório de concorrência. Crate independente, fora do pacote publicado.

## [0.1.0] - 2026-10-09

### Adicionado

#### Armazenamento e durabilidade
- **Buffer pool** com latches (`Arc<Frame>` + `RwLock`), *pin* por contagem de `Arc`, despejo por relógio
  e política **no-steal** ([ADR-0002](docs/adr/0002-no-steal-no-force-wal-redo.md)).
- **WAL de redo** com **commit atômico** (um registro com CRC-32 por transação), **checkpoint *sharp*** e
  recuperação **idempotente** (descarta registro final cortado ou corrompido)
  ([ADR-0003](docs/adr/0003-commit-atomico-um-registro.md)).
- **Índices reconstruídos** na recuperação, sem log próprio ([ADR-0004](docs/adr/0004-indices-reconstruidos-na-recuperacao.md)).
- **Operações estruturais** do heap sobrevivem ao rollback ([ADR-0005](docs/adr/0005-operacoes-estruturais-sobrevivem-ao-rollback.md)).
- **Heap**: compactação de página, reaproveitamento de slots, `hint` de espaço livre.
- Formato de arquivo `RUSTYSQ2` (o antigo `RUSTYSQL` é recusado).

#### Índices e SQL
- **B+tree concorrente** com ***latch crabbing*** e guards `Arc` (`parking_lot`)
  ([ADR-0006](docs/adr/0006-btree-latch-crabbing.md)); `verify()` confere todos os invariantes.
- **Chaves *memcomparable*** ([ADR-0007](docs/adr/0007-chaves-memcomparable.md)).
- **Índices secundários e únicos** (`CREATE [UNIQUE] INDEX`, `DROP INDEX`) e buscas por **intervalo**.
- **Planejador por regras**: escolha de índice, *pushdown*, junção por índice / *hash* / laço aninhado
  ([ADR-0015](docs/adr/0015-planejador-por-regras-e-executor-volcano.md)); `EXPLAIN`.
- `JOIN` e `LEFT JOIN`, `GROUP BY`, `HAVING`, `COUNT/SUM/AVG/MIN/MAX`, `ORDER BY` (alias e posição),
  `LIMIT/OFFSET`, `LIKE`, `BETWEEN`, `IN`, operador `%`, alias de tabela e colunas qualificadas.

#### Transações e concorrência
- **Transações** `BEGIN`/`COMMIT`/`ROLLBACK`, com rollback em memória e **atomicidade por comando**
  ([ADR-0013](docs/adr/0013-rollback-em-memoria-e-atomicidade-por-comando.md)).
- **Locks de tabela** (*strict 2PL*) com **detecção de deadlock** e **preferência a escritores**
  ([ADR-0009](docs/adr/0009-locks-de-tabela-strict-2pl.md), [ADR-0011](docs/adr/0011-preferencia-a-escritores.md)).
- **Gerenciador de prioridade** (`sched`): fila de admissão com `LOW/NORMAL/HIGH`, ***aging***,
  `SET PRIORITY` e cessão de vaga ao bloquear em lock
  ([ADR-0012](docs/adr/0012-controle-de-admissao-com-prioridade.md)).
- API de sessões: `Database` (`Clone`, `Send + Sync`) e `Session`.
- DDL em autocommit, publicado no commit ([ADR-0014](docs/adr/0014-ddl-em-autocommit-e-publicacao-no-commit.md)).

#### Comunidade e infraestrutura
- **CI** no GitHub Actions: testes em Linux, Windows e macOS; `clippy` e `rustdoc` sem avisos; verificação
  da **versão mínima do Rust (1.88)**; links da documentação; e um **estresse semanal** de concorrência.
- Modelos de *issue* (bug e funcionalidade), de *pull request*, `CODE_OF_CONDUCT.md`, `SECURITY.md` e
  Dependabot.
- `rust-version = "1.88"` e metadados do pacote (`keywords`, `categories`) no `Cargo.toml`.

#### Ferramentas e documentação
- REPL com `.stats`, `.locks`, `.schema`, modo script com tempos por comando.
- **rustdoc** em todo item público (`#![warn(missing_docs)]`), com **exemplos executáveis**.
- Documentação em `docs/`: arc42, diagramas C4, cenários de execução, **15 ADRs** (MADR), referência por
  módulo, referência de SQL / formatos / erros / configuração / CLI, guias e glossário.
- `examples/storage_dump.rs`: mostra os bytes reais do WAL e das páginas.
- `scripts/check_docs.py`: verifica links e âncoras da documentação.
- **52 testes**: 1 unitário, 44 de integração (inclui estresse de concorrência da B-tree, transferências
  bancárias com deadlock, recuperação de quedas) e 7 exemplos de rustdoc.

### Licença
- O projeto passa a ser distribuído sob a **licença MIT** ([`LICENSE`](LICENSE)); o `Cargo.toml` declara
  `license = "MIT"`.

### Alterado

- O armazenamento passou a usar **buffer pool** em vez de acesso direto ao arquivo (`pager`).
- O catálogo passou a ter dois tipos de linha, `table` e `index`; o índice da chave primária (`pk_<tabela>`)
  é um índice como os outros, e os secundários usam o mesmo mecanismo.
- Inserções em lote ficaram ~6× mais rápidas (20 mil linhas: 3,06 s → 0,48 s) graças à dica de espaço
  livre do heap e ao buffer pool.

### Corrigido

- **Inanição de escritores por vítima de deadlock errada**: o gerenciador abortava sempre quem fechava o
  ciclo, e o escritor perdia para leitores ociosos; o teste de transferências travava por mais de 20 s
  (entre 11 mil e 62 mil *retries*). Agora a vítima é escolhida **entre os membros do ciclo** por
  prioridade, trabalho feito e idade: **61 *retries***
  ([ADR-0010](docs/adr/0010-vitima-de-deadlock.md)).
- **Arestas obsoletas** no grafo de espera de locks podiam fechar ciclos falsos.
- **Perda de linhas commitadas** quando uma transação abortada tinha alocado páginas novas e outra
  transação escrevia nelas depois: as operações estruturais agora vão ao WAL mesmo no rollback
  ([ADR-0005](docs/adr/0005-operacoes-estruturais-sobrevivem-ao-rollback.md)).
- O operador `%` não existia.

### Removido

- `pager.rs` (substituído por `disk.rs` + `bufferpool.rs`).
- Formato de arquivo antigo (`RUSTYSQL`): não há migração.

## Histórico anterior

### Base inicial (`3aff447`)

- Pager de páginas de 4 KB, heap em *slotted pages*, B+tree **sem concorrência** para a chave primária,
  parser SQL (`CREATE TABLE`, `INSERT`, `SELECT`, `UPDATE`, `DELETE`) e REPL.

[Não lançado]: https://github.com/snake-outerheaven/rustysql/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/snake-outerheaven/rustysql/releases/tag/v0.1.0
