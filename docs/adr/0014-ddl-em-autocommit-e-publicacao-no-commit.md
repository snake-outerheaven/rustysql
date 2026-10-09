# ADR-0014: DDL só em autocommit, serializado, publicado no commit

* **Status**: aceita
* **Data**: 2026-10-09
* **Módulos afetados**: `db`, `catalog`, `txn`

## Contexto e declaração do problema

`CREATE TABLE`, `DROP TABLE`, `CREATE INDEX` e `DROP INDEX` mudam o **catálogo**: a lista de tabelas e
índices que todos os comandos consultam. Precisamos decidir (1) se DDL pode participar de uma
transação explícita; (2) como evitar que outra thread veja uma tabela pela metade; (3) o que acontece
com o catálogo em memória se o DDL falhar ou der rollback.

O catálogo é uma tabela comum (heap na página 1; veja
[`reference/storage-formats.md`](../reference/storage-formats.md#catálogo)), então suas linhas já passam
pelo WAL como qualquer outra. O desafio é o catálogo **em memória** (`Catalog`, `TableInfo`,
`IndexInfo`), derivado dele.

## Fatores de decisão

* nenhuma thread pode ver uma tabela cujo `CREATE` não commitou, nem continuar vendo uma cujo `DROP` commitou;
* um `CREATE INDEX` precisa ler a tabela inteira sem que mude durante a construção;
* simplicidade.

## Opções consideradas

1. **DDL transacional completo**: DDL dentro de `BEGIN`, com visibilidade por transação do catálogo.
2. **DDL em autocommit**: cada DDL é a sua própria transação; recusado dentro de `BEGIN`.
3. DDL sem transação (alterar o catálogo em memória na hora).

## Resultado da decisão

Escolhida a **opção 2**.

* `Inner::begin_ddl` rejeita DDL em transação explícita (`Error::Exec`) e pega o **guard de DDL**
  (`Arc<Mutex<()>>::lock_arc`), guardado no `Txn`: **um DDL por vez**, até o fim da transação.
* A mudança no **catálogo em disco** (linhas do heap da página 1) é feita normalmente, dentro da
  transação, com undo e WAL.
* A mudança no **catálogo em memória** é adiada: o DDL registra uma closure em `Txn::on_commit`, que
  roda **depois** do registro de WAL estar durável e antes de soltar os locks. Se a transação der
  rollback, a closure nunca roda.
* O DDL pega `Exclusive` na tabela afetada: espera as transações que a usam terminarem. Quem estava
  esperando um lock de uma tabela que foi removida **revalida** `TableInfo::dropped` ao acordar.
* `CREATE INDEX` constrói a árvore dentro da transação, lendo a tabela sob o lock exclusivo, e só a
  publica em `TableInfo::indexes` no commit.

### Consequências

* **Bom**: nenhum estado intermediário de catálogo é visível; rollback de DDL é gratuito.
* **Bom**: reaproveita 100% o caminho de WAL, undo e recuperação das linhas comuns.
* **Bom**: um `CREATE INDEX` falho (valores duplicados num índice único) deixa o banco intacto; o nome
  fica livre de novo.
* **Ruim**: não dá para agrupar DDL e DML numa transação ("criar tabela e popular, tudo ou nada").
* **Ruim**: as páginas alocadas por um DDL abortado ficam órfãs (como em D4).
* **Ruim**: um DDL espera por qualquer transação aberta que use a tabela.

## Validação

| Teste | Garante |
|---|---|
| `txn::a_failing_statement_does_not_abort_the_transaction` | `BEGIN; CREATE TABLE` é recusado |
| `sql::unique_index_and_statement_atomicity` | `CREATE UNIQUE INDEX` sobre duplicatas falha e não deixa o índice |
| `sql::update_changing_primary_keys_in_bulk_and_ddl_errors` | `DROP TABLE` e recriação com o mesmo nome |
| `recovery::ddl_survives_a_crash` | tabelas e índices criados/removidos sobrevivem à queda |

## Links

* [`modules/db.md`](../modules/db.md), [`modules/catalog.md`](../modules/catalog.md)
