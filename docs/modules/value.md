# Módulo `value`

> Tipos de dados, valores SQL, linhas e a serialização de linhas.

| | |
|---|---|
| **Arquivo** | `src/value.rs` |
| **Camada** | dados |
| **Depende de** | `error` |
| **Usado por** | `ast`, `catalog`, `db`, `eval`, `exec`, `keyenc`, `parser`, `plan`, `schema` |
| **Testes** | indireto (todos os testes de SQL); formato verificado por `basic::persistence_and_many_pages` |

## 1. Responsabilidade

Definir `DataType`, `Value` e `Row`; comparar valores (com `NULL`); serializar/desserializar linhas
para o heap. **Não faz** aritmética nem lógica (é do [`eval`](eval.md)).

## 2. API pública

| Item | Descrição |
|---|---|
| `enum DataType { Int, Float, Text, Bool }` | tipos de coluna; `name()`, `parse(&str)` |
| `enum Value { Null, Int(i64), Float(f64), Text(String), Bool(bool) }` | um valor |
| `type Row = Vec<Value>` | uma linha |
| `Value::sql_cmp(&self, &Value) -> Option<Ordering>` | comparação **SQL**: `None` se envolve `NULL` ou tipos incompatíveis |
| `Value::total_cmp(&self, &Value) -> Ordering` | ordem **total** (para `ORDER BY`, `MIN`/`MAX`): `NULL` é o menor |
| `encode_row(&[Value]) -> Vec<u8>` | serializa |
| `decode_row(&[u8]) -> Row` | desserializa |
| `impl Display for Value` | `NULL`, número, texto cru, `true`/`false` |

## 3. Estruturas de dados

**Aliases de tipo** aceitos pelo parser (`DataType::parse`, sem diferenciar caixa):

| Tipo | Aliases |
|---|---|
| `INT` | `INTEGER`, `BIGINT` |
| `FLOAT` | `REAL`, `DOUBLE` |
| `TEXT` | `VARCHAR`, `STRING` (o `(n)` de `VARCHAR(n)` é aceito e **ignorado**) |
| `BOOL` | `BOOLEAN` |

**Formato de uma linha** (`encode_row`): para cada valor, um byte de *tag* e o conteúdo:

| Tag | Valor | Conteúdo |
|---:|---|---|
| 0 | `Null` | nada |
| 1 | `Int` | 8 bytes little-endian |
| 2 | `Float` | 8 bytes little-endian (IEEE-754) |
| 3 | `Text` | `u32` little-endian com o tamanho + bytes UTF-8 |
| 4 | `Bool` | 1 byte (0/1) |

A linha **não** guarda o número de colunas nem os tipos: quem lê conhece o esquema (a linha termina
quando os bytes acabam).

## 4. Algoritmos e fluxos

**`sql_cmp`**: `Int×Int`, `Float×Float`, `Text×Text` (bytes UTF-8), `Bool×Bool` e `Int×Float` (convertendo
o inteiro para `f64`, o que pode perder precisão acima de 2⁵³). Qualquer outra combinação, ou `NULL`,
devolve `None`. `None` vira "desconhecido" nas expressões (um erro, se os tipos são incompatíveis).

**`total_cmp`**: `NULL = NULL`; `NULL` menor que tudo; senão `sql_cmp`, e tipos incomparáveis contam
como iguais (não deveria ocorrer numa coluna tipada).

## 5. Invariantes

* `decode_row(encode_row(r)) == r` para qualquer linha.
* `NULL` nunca é igual a nada em `sql_cmp` (inclusive a outro `NULL`); em `PartialEq` (derivado),
  `Null == Null` é `true` (é igualdade estrutural, usada em testes e em `IS NULL`).

## 6. Concorrência

Tipos de dados puros, `Send + Sync`.

## 7. Erros

`decode_row` devolve `Error::Storage` para registro truncado, tag desconhecida ou UTF-8 inválido.

## 8. Testes

Indireto. O contrato de ordem de `total_cmp` é exercitado por `ORDER BY` em
`sql::aggregates_group_by_having_order_limit` e `sql::joins_choose_the_right_algorithm_and_compute_correctly`.

## 9. Limitações e evolução

* Sem tipos de data/hora, decimal exato (`NUMERIC`) nem binário (`BLOB`).
* `Int × Float` perde precisão para inteiros grandes.
* Texto sem *collation*.
* O formato de linha não tem versão: mudar um tipo exige migração.

## 10. Veja também

[`schema`](schema.md), [`eval`](eval.md), [`keyenc`](keyenc.md) (codificação **de chave**, diferente da de linha),
[`../reference/storage-formats.md`](../reference/storage-formats.md#linha-registro-do-heap)
