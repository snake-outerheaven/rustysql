# Módulo `catalog`

> Tabelas e índices em memória, e como o catálogo é guardado em disco.

| | |
|---|---|
| **Arquivo** | `src/catalog.rs` |
| **Camada** | motor |
| **Depende de** | `btree`, `heap`, `keyenc`, `lockmgr`, `schema`, `value`; `parking_lot` |
| **Usado por** | `db`, `plan`, `exec` |
| **Testes** | `recovery::ddl_survives_a_crash`, `sql::*` (índices, DDL) |

## 1. Responsabilidade

Guardar as estruturas que o executor precisa: para cada tabela, o seu esquema, o `HeapFile` e a lista
de índices (`BTree`). É uma **visão em memória** do catálogo persistente (uma tabela comum), reconstruída
na abertura do banco.

## 2. API pública

| Item | Descrição |
|---|---|
| `TableInfo { name, schema, heap, indexes, catalog_rid, dropped }` | uma tabela |
| `TableInfo::id()` | `heap.root` (identifica a tabela e **nunca** é reaproveitado) |
| `TableInfo::indexes()`, `index_on(col)` | cópia da lista de índices; índices de uma coluna |
| `IndexInfo { name, table, column, unique, tree, catalog_rid }` | um índice |
| `IndexInfo::key_for(&row, rid)` | a chave desta linha neste índice |
| `Catalog::get`, `publish`, `remove`, `all`, `find_index` | acesso ao mapa de tabelas |

## 3. Estruturas de dados

**Catálogo em disco**: o heap da **página 1** guarda uma linha por tabela e uma por índice, codificadas
como qualquer linha (`encode_row`):

```
("table", nome, raiz_do_heap: Int, esquema: Text)
("index", nome, tabela, coluna: Int, único: Bool, raiz_da_btree: Int)
```

**Em memória**: `Catalog { tables: RwLock<HashMap<String, Arc<TableInfo>>> }`. `TableInfo::indexes` está
atrás de um `RwLock` porque `CREATE INDEX`/`DROP INDEX` alteram a lista **depois** de a tabela ser
publicada. `TableInfo::dropped` é um `AtomicBool` marcado quando um `DROP TABLE` commita.

**Nomes**: o índice da chave primária se chama `pk_<tabela>` e é criado junto com a tabela.

## 4. Algoritmos e fluxos

**`IndexInfo::key_for(row, rid)`**:
* índice **único** e valor **não nulo** → `value_key(v)` (a B-tree recusa duplicatas);
* senão → `value_rid_key(v, rid)` (não único, ou `NULL` num índice único: vários `NULL`s são permitidos).

**Publicação** (`publish`): só acontece **depois do commit durável**, via `Txn::on_commit`
([ADR-0014](../adr/0014-ddl-em-autocommit-e-publicacao-no-commit.md)).

**Carga** (`Inner::load_catalog`, em `db.rs`): varre o heap da página 1; cria os `TableInfo` e depois
associa os `IndexInfo`.

## 5. Invariantes

1. Nome de tabela é único; nome de índice é único **no banco** (checado por `find_index`).
2. Uma tabela publicada tem exatamente os índices que estão no catálogo em disco.
3. `TableInfo::id()` é estável durante a vida da tabela.
4. Se `dropped` é `true`, a tabela já saiu do mapa (transações que esperavam o lock revalidam).

## 6. Concorrência

`Catalog` é `Send + Sync`: um `RwLock` curto sobre o mapa. As **mudanças** de DDL são serializadas pelo
guard de DDL e publicadas só no commit, então os leitores veem o catálogo antes ou depois do DDL, nunca
no meio.

## 7. Erros

Nenhum próprio (estruturas passivas); os erros de DDL são gerados em `db.rs`.

## 8. Testes

`recovery::ddl_survives_a_crash` (tabela, índices único e comum, `DROP`, recriação), `sql::secondary_index_*`,
`sql::unique_index_and_statement_atomicity`.

## 9. Limitações e evolução

* Não há versionamento de esquema nem `ALTER TABLE`.
* O catálogo em disco não tem checksum próprio (depende do heap/WAL).
* Um `HashMap` global sob `RwLock`; milhares de tabelas pediriam outra estrutura.

## 10. Veja também

[`schema`](schema.md), [`heap`](heap.md), [`btree`](btree.md), [`db`](db.md), [ADR-0014](../adr/0014-ddl-em-autocommit-e-publicacao-no-commit.md),
[`../reference/storage-formats.md`](../reference/storage-formats.md#catálogo)
