# Módulo `parser`

> Parser descendente recursivo: tokens → `Statement`.

| | |
|---|---|
| **Arquivo** | `src/parser.rs` |
| **Camada** | SQL |
| **Depende de** | `ast`, `error`, `lexer`, `sched` (para `Priority`), `schema`, `value` |
| **Usado por** | `db` |
| **Testes** | *doctest* de `parser::parse`; `sql::*`, `txn::*`, `sched::set_priority_*` |

## 1. Responsabilidade

Aplicar a gramática ([`../reference/sql.md`](../reference/sql.md#gramática)) sobre os tokens e devolver
`Vec<Statement>`. Normaliza identificadores para minúsculas e **desaçucara** algumas construções em
expressões mais simples.

## 2. API pública

| Item | Descrição |
|---|---|
| `parse(&str) -> Result<Vec<Statement>>` | interpreta um ou mais comandos separados por `;` |

(`Parser` é privado.)

## 3. Estruturas de dados

`struct Parser { toks: Vec<Token>, pos: usize }` com os utilitários `peek`, `peek_at(n)`, `next`,
`eat(&Token)`, `expect(Token)`, `peek_kw/eat_kw/expect_kw` (palavras-chave sem diferenciar caixa),
`ident()` (devolve minúsculas) e `peek_alias()`.

## 4. Algoritmos e fluxos

**Estrutura**: um método por regra da gramática: `statement` → `create`, `create_table`, `insert`,
`select`, `update`, ...; expressões em cascata de precedência:

```
expr  →  and_expr ( OR and_expr )*
and_expr → not_expr ( AND not_expr )*
not_expr → NOT not_expr | comparison
comparison → additive [ IS [NOT] NULL
                       | [NOT] BETWEEN additive AND additive
                       | [NOT] IN ( additive , ... )
                       | [NOT] LIKE additive
                       | (= | <> | < | <= | > | >=) additive ]
additive → multiplicative ( (+|-) multiplicative )*
multiplicative → unary ( (*|/|%) unary )*
unary → - unary | primary
primary → número | texto | TRUE | FALSE | NULL | ( expr ) | coluna | tabela.coluna | agregação
```

**Desaçucaramento** (o resto do sistema nunca vê estas formas):

| Escrito | Vira |
|---|---|
| `x BETWEEN a AND b` | `x >= a AND x <= b` |
| `x IN (a, b, c)` | `x = a OR x = b OR x = c` |
| `x NOT BETWEEN/IN/LIKE ...` | `NOT (...)` |
| `BEGIN [TRANSACTION|WORK]` | `Statement::Begin` |

**Aliases**: depois do nome de uma tabela, um identificador que **não** esteja na lista de palavras
reservadas é um alias (`FROM emp e`); `AS` também é aceito. Reservadas: `select from where group order
by having limit offset join inner left outer on set values and or not as union`.

**`parse`** repete: pula `;` soltos; se acabou, termina; interpreta um comando; exige `;` ou o fim.

## 5. Invariantes

* Todo identificador no resultado está em minúsculas.
* `BETWEEN` e `IN` nunca chegam ao planejador.
* Uma entrada sem tokens produz `Ok(vec![])`.

## 6. Concorrência

Função pura sobre os próprios dados: thread-safe.

## 7. Erros

`Error::Parse` com mensagens como `esperava FROM, encontrei ...`, `comando desconhecido: ...`,
`tipo desconhecido 'x'`, `função desconhecida 'x'`, `DISTINCT em agregações não é suportado`,
`prioridade desconhecida 'x' (use LOW, NORMAL ou HIGH)`, `UNIQUE só vale para CREATE INDEX`.

## 8. Testes

O *doctest* de `parse` (3 comandos; `SELEC` falha), e todos os testes de SQL dos arquivos `tests/*.rs`.
A expressividade (precedência, `BETWEEN`, `IN`, `LIKE`, `NOT`, aliases, `JOIN`) está em
`sql::expressions_like_between_in` e `sql::joins_choose_the_right_algorithm_and_compute_correctly`.

## 9. Limitações e evolução

* Erros sem posição (linha/coluna) e sem recuperação (para no primeiro).
* Sem `SELECT` sem `FROM`, subconsultas, `DISTINCT`, `UNION`, `CASE`, `CROSS JOIN`, `RIGHT/FULL JOIN`,
  `USING`, junção por vírgula (`FROM a, b`).
* Palavras-chave não são reservadas **em geral**: só a lista acima impede alias (uma coluna pode se chamar
  `count` ou `key`).

## 10. Veja também

[`lexer`](lexer.md), [`ast`](ast.md), [`../reference/sql.md`](../reference/sql.md)
