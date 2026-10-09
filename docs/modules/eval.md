# Módulo `eval`

> Expressões: ligação de colunas (`bind`), avaliação com lógica de três valores e `LIKE`.

| | |
|---|---|
| **Arquivo** | `src/eval.rs` |
| **Camada** | SQL |
| **Depende de** | `ast`, `error`, `schema`, `value` |
| **Usado por** | `plan`, `exec`, `db` |
| **Testes** | `sql::expressions_like_between_in`, `sql::aggregates_*`, `sql::update_changing_primary_keys_*` (aritmética e `%`) |

## 1. Responsabilidade

Avaliar expressões sobre uma linha. O módulo faz **duas fases**:

1. **`bind`** (uma vez por comando): resolve nomes de coluna para **posições** na linha, transformando
   `Expr` em `BExpr`. Erros de coluna desconhecida/ambígua aparecem aqui, antes de ler qualquer linha.
2. **`BExpr::eval`** (por linha): indexa o vetor; sem busca por nome.

## 2. API pública

| Item | Descrição |
|---|---|
| `Scope`, `ScopeCol { table, name }` | colunas visíveis, na ordem em que aparecem na linha |
| `Scope::for_table(alias, &schema)` | escopo de uma tabela inteira |
| `Scope::concat(&other)` | junta escopos (resultado de um join) |
| `Scope::resolve(table, name) -> usize` | posição da coluna (erro se desconhecida ou ambígua) |
| `enum BExpr` | `Lit`, `Col(usize)`, `Unary`, `Binary`, `IsNull`, `Like` |
| `bind(&Expr, &Scope) -> BExpr` | liga a expressão ao escopo |
| `BExpr::eval(&[Value]) -> Value` | avalia |
| `BExpr::test(&[Value]) -> bool` | avalia como filtro: só `TRUE` passa (`NULL` e `FALSE` não) |
| `const_value(&Expr) -> Option<Value>` | valor de uma expressão **sem colunas** (para o planejador) |

## 3. Estruturas de dados

`Scope.cols: Vec<ScopeCol>`. Colunas **sintéticas** (resultado de agregações) têm `table = ""` e nome
`#agg0`, `#agg1`... (o `#` não é um caractere legal de identificador, então nunca colide com uma coluna
do usuário).

## 4. Algoritmos e fluxos

### `resolve`
Coleta as posições cujo nome casa (e cujo qualificador casa, se dado). **Zero** → "coluna desconhecida
'x'"; **mais de uma** → "coluna 'x' é ambígua; qualifique com o nome da tabela"; **uma** → posição.

### Lógica de três valores

`NOT`: `NULL → NULL`.

| `AND` | `T` | `F` | `NULL` |
|---|---|---|---|
| **`T`** | T | F | NULL |
| **`F`** | F | F | F |
| **`NULL`** | NULL | F | NULL |

| `OR` | `T` | `F` | `NULL` |
|---|---|---|---|
| **`T`** | T | T | T |
| **`F`** | T | F | NULL |
| **`NULL`** | T | NULL | NULL |

**Comparações** (`=`, `<>`, `<`, `<=`, `>`, `>=`): se algum lado é `NULL` → `NULL`; senão usa
`Value::sql_cmp`; tipos incompatíveis (ex.: `INT` com `TEXT`) → **erro**, não `FALSE`.

**`IS [NOT] NULL`** é o único teste que enxerga `NULL` como valor; nunca devolve `NULL`.

### Aritmética
| Operandos | Resultado |
|---|---|
| `NULL` em qualquer lado | `NULL` |
| `INT op INT` | `INT` com **overflow checado** (erro); `/` e `%` por zero → erro |
| `INT`/`FLOAT` misturados | `FLOAT` |
| `TEXT + TEXT` | concatenação |
| qualquer outra combinação | erro |

### `LIKE`
Algoritmo de dois ponteiros com retrocesso na última `%` (O(n·m) no pior caso, sem recursão):
`%` casa qualquer sequência (inclusive vazia), `_` casa exatamente um caractere (Unicode), o resto casa
literalmente (**diferencia maiúsculas**). `NULL` em qualquer lado → `NULL`; operandos não-texto → erro.

## 5. Invariantes

* `bind` nunca devolve uma expressão com agregação (`Error::Exec("função de agregação não é permitida
  neste contexto")`): o planejador extrai as agregações antes.
* Depois do `bind`, `eval` só falha por erro **de dados** (divisão por zero, tipos, overflow), nunca por nome.

## 6. Concorrência

Funções puras; `BExpr` é `Send + Sync`.

## 7. Erros

`Error::Exec`: coluna desconhecida/ambígua, agregação fora de lugar, divisão por zero, overflow, tipos
incompatíveis, `WHERE` não booleano.

## 8. Testes

`sql::expressions_like_between_in` (`LIKE`, `NOT LIKE`, `BETWEEN`, `IN`, `IS NULL`, `NULL` em comparações,
aliases, `%`), `sql::aggregates_group_by_having_order_limit` (aritmética sobre agregados),
`sql::joins_*` (ambiguidade de coluna).

## 9. Limitações e evolução

* Sem funções escalares (`UPPER`, `COALESCE`, `LENGTH`), sem `CASE`, sem `CAST`.
* `Int × Float` converte o inteiro para `f64` (perde precisão acima de 2⁵³).
* `LIKE` sem escape e sem `ILIKE`.

## 10. Veja também

[`ast`](ast.md), [`plan`](plan.md), [`exec`](exec.md), [`value`](value.md), [`../reference/sql.md`](../reference/sql.md#expressões-e-null)
