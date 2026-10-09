# Módulo `schema`

> Colunas de uma tabela e a sua serialização para o catálogo.

| | |
|---|---|
| **Arquivo** | `src/schema.rs` |
| **Camada** | dados |
| **Depende de** | `error`, `value` |
| **Usado por** | `ast`, `catalog`, `db`, `eval`, `parser`, `plan` |
| **Testes** | `recovery::ddl_survives_a_crash` (serialização/leitura do esquema), `sql::*` |

## 1. Responsabilidade

Descrever as colunas (`Column`) e o esquema (`Schema`), achar colunas pelo nome e converter o
esquema de/para o texto guardado no catálogo.

## 2. API pública

| Item | Descrição |
|---|---|
| `Column { name, ty, not_null, primary_key }` | uma coluna |
| `Schema { columns }` | as colunas, na ordem |
| `Schema::index(name) -> Option<usize>` | posição da coluna |
| `Schema::primary_key() -> Option<usize>` | posição da chave primária |
| `Schema::serialize() -> String` | texto para o catálogo |
| `Schema::deserialize(&str) -> Schema` | inverso (`Error::Storage` se malformado) |

## 3. Estruturas de dados

**Formato serializado**: colunas separadas por `;`, cada uma com quatro campos separados por `,`:

```
id,INT,1,1;nome,TEXT,0,0;saldo,FLOAT,0,0
└─┬┘ └┬┘ │ └ primary_key (0/1)
  │   │  └── not_null (0/1)
  │   └───── tipo (DataType::name)
  └───────── nome da coluna
```

Por isso **nomes de coluna não podem conter `,` ou `;`**: o lexer só gera identificadores
alfanuméricos e `_`, então isso nunca acontece pelo SQL.

## 4. Algoritmos e fluxos

`index` e `primary_key` são buscas lineares (esquemas têm poucas colunas). Chave primária implica
`not_null` (o parser marca as duas flags).

## 5. Invariantes

* No máximo uma coluna com `primary_key = true` (verificado em `create_table`).
* `deserialize(serialize(s)) == s`.

## 6. Concorrência

Imutável depois de criado (vive dentro de um `Arc<TableInfo>`): `Send + Sync`.

## 7. Erros

`deserialize` → `Error::Storage("esquema corrompido no catálogo")`.

## 8. Testes

`recovery::ddl_survives_a_crash`: o esquema é gravado no catálogo, a queda acontece, e o esquema volta
da leitura do heap na recuperação.

## 9. Limitações e evolução

* Sem valores padrão (`DEFAULT`), `CHECK`, chaves estrangeiras, nem `ALTER TABLE` (o esquema nunca muda).
* Uma só chave primária, de uma só coluna.
* O formato é textual e sem versão.

## 10. Veja também

[`catalog`](catalog.md), [`value`](value.md), [`../reference/storage-formats.md`](../reference/storage-formats.md#catálogo)
