# Módulo `exec`

> Executor: transforma um `Plan` numa cadeia de iteradores (modelo *Volcano*).

| | |
|---|---|
| **Arquivo** | `src/exec.rs` |
| **Camada** | SQL |
| **Depende de** | `ast`, `btree`, `catalog`, `error`, `eval`, `heap`, `keyenc`, `plan`, `value` |
| **Usado por** | `db` |
| **Testes** | `sql::*` (todos), `txn::*` |

## 1. Responsabilidade

Dado um `Plan`, produzir as linhas. Cada operador é um iterador que **puxa** linhas do filho; os que
precisam ver tudo antes de emitir **materializam** a entrada. Também fornece `collect_rids` para
`UPDATE`/`DELETE` (linhas **com** `RowId`).

## 2. API pública

| Item | Descrição |
|---|---|
| `execute(&Plan) -> RowIter` | monta a cadeia de iteradores |
| `type RowIter = Box<dyn Iterator<Item = Result<Row>>>` | fluxo de linhas; cada elemento pode ser erro |
| `collect_rids(&Plan) -> Vec<(RowId, Row)>` | linhas que satisfazem um scan (+ `Filter`), com endereço |

## 3. Estruturas de dados

Operadores (cada `Node` do plano):

| Operador | Fluxo ou materializa? | Implementação |
|---|---|---|
| `SeqScan` | fluxo | `HeapFile::scan` + `decode_row` |
| `IndexScan` | fluxo | `BTree::scan(start, end)` e, para cada `RowId`, `HeapFile::get` |
| `Filter` | fluxo | `filter_map` com `BExpr::test` |
| `Project` | fluxo | avalia as expressões da lista |
| `Limit` | fluxo | `skip(offset).take(limit)` |
| `Sort` | **materializa** | calcula as chaves uma vez por linha; `sort_by` **estável** (`Value::total_cmp`) |
| `Aggregate` | **materializa** | tabela hash `chave do grupo → (primeira linha, acumuladores)` |
| `NestedLoop` | materializa o **lado direito** | para cada linha da esquerda, percorre a direita |
| `HashJoin` | materializa o **lado direito** | tabela hash `chave → linhas`; `NULL` nunca casa |
| `IndexJoin` | fluxo na esquerda | por linha da esquerda, busca no índice da direita e lê as linhas pelo `RowId` |

Acumuladores (`enum Acc`): `Count`, `Sum{int,float,saw_float,any}`, `Avg{sum,n}`, `Min`, `Max`.

## 4. Algoritmos e fluxos

**Junções** compartilham `join_row(left, candidatos, predicado, tipo, largura_direita)`: combina a linha
da esquerda com cada candidato, testa o predicado residual, e — num `LEFT JOIN` sem nenhum casamento —
emite a linha da esquerda estendida com `NULL`s.

**`Aggregate`**: a **chave do grupo** é `encode_row` dos valores das expressões do `GROUP BY` (então
`NULL`s caem no mesmo grupo). A **saída** é a *primeira linha do grupo* seguida dos valores agregados
(`#agg0`, `#agg1`...), de modo que expressões sobre colunas do `GROUP BY` continuam funcionando depois do
agrupamento. Agregação **global** (sem `GROUP BY`) sobre zero linhas ainda emite **uma** linha
(`COUNT = 0`, o resto `NULL`).

**Semântica dos acumuladores**:

| Função | Ignora `NULL`? | Vazio | Tipo |
|---|---|---|---|
| `COUNT(*)` | conta tudo | 0 | `INT` |
| `COUNT(x)` | sim | 0 | `INT` |
| `SUM(x)` | sim | `NULL` | `INT` (overflow é erro) ou `FLOAT` se algum valor é decimal |
| `AVG(x)` | sim | `NULL` | `FLOAT` |
| `MIN/MAX(x)` | sim | `NULL` | o do argumento (ordem de `total_cmp`) |

**`collect_rids`**: para `Filter{Scan}` percorre o scan aplicando o predicado; para um scan simples,
devolve tudo. `UPDATE` e `DELETE` **materializam** o resultado antes de modificar nada.

## 5. Invariantes

* Um erro durante a iteração chega como `Err` no elemento e **não** interrompe quem não olha para ele (o
  chamador, `Inner::select`, interrompe no primeiro `Err` ao fazer `collect`).
* `IndexScan` que aponta para um registro inexistente é erro (`Error::Storage("índice aponta para
  registro inexistente")`): sinaliza índice inconsistente.

## 6. Concorrência

Os iteradores são `'static` e **não** seguram latches entre elementos (cada `scan` solta o latch ao fim
de cada página). A consistência durante a execução vem dos **locks de tabela** pegos antes
(`Inner::select`).

## 7. Erros

`Error::Exec` (divisão por zero, tipos incompatíveis, `SUM` de não numérico, `WHERE` não booleano);
`Error::Storage` (índice inconsistente, página inexistente).

## 8. Testes

`sql::*` cobrem todos os operadores; `sql::joins_choose_the_right_algorithm_and_compute_correctly`
verifica o **resultado** de cada algoritmo de junção; `sql::aggregates_group_by_having_order_limit` os
acumuladores, incluindo agregação global vazia.

## 9. Limitações e evolução

* `Sort`, `Aggregate` e o lado direito das junções precisam caber na memória (sem *spill* para disco).
* Sem paralelismo dentro de uma consulta.
* Sem *merge join* nem *sort-merge group by*.
* O `HashJoin` só entende igualdade de chaves de **mesmo tipo** (`INT` com `FLOAT` não casam; o planejador
  evita esse caso).

## 10. Veja também

[ADR-0015](../adr/0015-planejador-por-regras-e-executor-volcano.md), [`plan`](plan.md), [`eval`](eval.md),
[`heap`](heap.md), [`btree`](btree.md)
