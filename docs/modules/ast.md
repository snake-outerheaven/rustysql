# Módulo `ast`

> Árvore sintática: o que o parser produz e o planejador consome.

| | |
|---|---|
| **Arquivo** | `src/ast.rs` |
| **Camada** | SQL |
| **Depende de** | `sched` (tipo `Priority`), `schema` (tipo `Column`), `value` |
| **Usado por** | `parser`, `eval`, `plan`, `exec`, `db` |
| **Testes** | indireto (parser e planejador) |

## 1. Responsabilidade

Definir os tipos que representam um comando SQL já interpretado, e alguns utilitários sobre
expressões. **Sem** lógica de execução.

## 2. API pública

| Item | Descrição |
|---|---|
| `enum Statement` | `CreateTable`, `DropTable`, `CreateIndex`, `DropIndex`, `Insert`, `Select`, `Update`, `Delete`, `Begin`, `Commit`, `Rollback`, `Checkpoint`, `SetPriority`, `Explain` |
| `struct SelectStmt` | `items`, `from`, `joins`, `filter`, `group_by`, `having`, `order_by`, `limit`, `offset` |
| `enum SelectItem` | `Star`, `QualifiedStar`, `Expr{expr, alias}` |
| `struct TableRef`, `struct Join`, `enum JoinKind` | `FROM` / `JOIN` (`Inner`, `Left`) |
| `enum Expr` | `Lit`, `Col{table,name}`, `Unary`, `Binary`, `IsNull`, `Like`, `Agg` |
| `enum UnOp`, `BinOp`, `AggFunc` | operadores e funções de agregação |
| `Expr::col`, `binary`, `contains_agg`, `columns`, `conjuncts`, `and_all` | utilitários |
| `impl Display for Expr` | imprime a expressão como SQL |

## 3. Estruturas de dados

* `Expr` é uma árvore com `Box`; **não** conhece colunas de nenhuma tabela (nomes ainda são strings).
* `Statement::Select(Box<SelectStmt>)` e `Explain(Box<Statement>)` usam `Box` para manter o `enum`
  pequeno.
* `Insert.rows` é `Vec<Vec<Expr>>`: os valores são **expressões** (podem ser aritmética sobre constantes).

## 4. Algoritmos e fluxos

| Utilitário | O que faz | Quem usa |
|---|---|---|
| `contains_agg()` | há agregação em algum ponto? | `plan` (decidir `Aggregate`), `eval::const_value` |
| `columns(&mut out)` | coleta `(qualificador, nome)` de todas as colunas | `plan` (pushdown, escolha de join) |
| `conjuncts()` | `a AND b AND c` → `[a, b, c]` | `plan` (pushdown), `db` (DML) |
| `and_all(parts)` | inverso de `conjuncts` | `plan` |

`Display` imprime `(a = 1)` com parênteses nos operadores binários, `NOT x`, `-x`, `x IS [NOT] NULL`,
`x [NOT] LIKE p`, `sum(x)` (nome da função em minúsculas), `count(*)`. É usado nos **nomes das colunas** de
saída (`SELECT idade + 1` → coluna `(idade + 1)`) e no `EXPLAIN`.

## 5. Invariantes

* Identificadores (tabela, coluna, alias) já vêm em **minúsculas** (o parser normaliza).
* `Expr::Agg { arg: None }` só existe para `COUNT(*)`.

## 6. Concorrência

Valores imutáveis depois de construídos; `Send + Sync`.

## 7. Erros

Nenhum (tipos de dados).

## 8. Testes

Coberto pelos testes de `parser` (`parse` doctest) e `sql::*`.

## 9. Limitações e evolução

* Sem posição no texto (linha/coluna) nos nós, o que impediria erros de execução apontarem para o SQL.
* Sem subconsultas, `CASE`, chamadas de função escalar, `CAST`, `DISTINCT`, `UNION`.

## 10. Veja também

[`parser`](parser.md), [`eval`](eval.md), [`plan`](plan.md), [`../reference/sql.md`](../reference/sql.md)
