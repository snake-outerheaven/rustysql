# Módulo `plan`

> Planejador por regras: `SelectStmt` → árvore de operadores físicos.

| | |
|---|---|
| **Arquivo** | `src/plan.rs` |
| **Camada** | SQL |
| **Depende de** | `ast`, `catalog`, `error`, `eval`, `keyenc`, `value` |
| **Usado por** | `exec`, `db` |
| **Testes** | `sql::secondary_index_*`, `sql::primary_key_uses_the_index_even_inside_and`, `sql::joins_*`, `sql::aggregates_*` |

## 1. Responsabilidade

Decidir **como** executar um `SELECT` (ou o scan de um `UPDATE`/`DELETE`): que índice usar, em que ordem
e por qual algoritmo juntar tabelas, onde aplicar cada filtro, como agregar, ordenar e projetar.
Racional: [ADR-0015](../adr/0015-planejador-por-regras-e-executor-volcano.md).

**Não faz**: executar (é do [`exec`](exec.md)), tomar locks, nem usar estatísticas (não há).

## 2. API pública

| Item | Descrição |
|---|---|
| `plan_select(&SelectStmt, &HashMap<String, Arc<TableInfo>>) -> Plan` | planeja um `SELECT` |
| `plan_table_scan(&Arc<TableInfo>, &Option<Expr>) -> Plan` | scan (com índice, se der) + filtro, para `UPDATE`/`DELETE` |
| `Plan { node, scope }` | um nó e o escopo das colunas que ele produz |
| `enum Node` | `SeqScan`, `IndexScan`, `Filter`, `NestedLoop`, `HashJoin`, `IndexJoin`, `Aggregate`, `Sort`, `Project`, `Limit` |
| `AggSpec { func, arg, name }` | uma agregação extraída (resultado em `#aggN`) |
| `Plan::explain() -> String` | o plano em texto, indentado |

## 3. Estruturas de dados

Um plano é uma árvore; cada `Plan` carrega o `Scope` da linha que produz (necessário porque o executor
**liga** as expressões ao escopo do filho). Exemplo de plano:

```
Project [d.nome, count(*)]                    ← Project   (escopo: nome, #agg0 renomeados)
  Aggregate group by [d.nome] compute [count(*)]
    IndexNestedLoopJoin Inner probe depts AS d using pk_depts with p.dept_id
      Filter (p.idade > 80)
        SeqScan pessoas AS p
```

## 4. Algoritmos e fluxos

### `plan_select`

1. **Relações**: `FROM` e cada `JOIN` viram uma lista de `(alias, TableInfo, tipo, condições do ON)`.
   Alias repetido → erro.
2. **Escopos**: um por tabela, mais o escopo completo (concatenação); e a faixa de colunas de cada tabela.
3. **Classificação dos conjuntos do `WHERE`** (`a AND b AND ...`): para cada um, `rel_set` calcula quais
   tabelas ele referencia (resolvendo as colunas no escopo completo; agregação → erro).
   * referencia **uma só** tabela `i`, e `i` é a 1ª ou é `INNER` → **desce** para o scan de `i`;
   * não referencia tabela alguma (constante) → filtro logo depois do 1º scan;
   * senão → filtro logo depois da junção que completa as tabelas referenciadas
     (**nunca** se empurra para o lado direito de um `LEFT JOIN`).
4. **Junções**, da esquerda para a direita. Para cada `JOIN k`:
   * conjuntos do `ON` que citam **só a tabela k** descem para o scan de `k` (vale também no `LEFT JOIN`);
   * procura uma igualdade `esq = dir` onde `esq` só usa tabelas anteriores e `dir` só a tabela `k`, com
     tipos compatíveis quando ambos são colunas simples;
   * **com igualdade e índice na coluna da direita** → `IndexJoin` (*index nested loop*);
   * **com igualdade e sem índice** → `HashJoin`;
   * **sem igualdade** → `NestedLoop`.
5. **Agregação**: se há `GROUP BY`, `HAVING` ou função de agregação em itens/`ORDER BY`, as chamadas são
   extraídas (`rewrite_aggs`, deduplicando as idênticas) para colunas `#aggN`, e o resto da consulta passa
   a referenciá-las.
6. **Itens do `SELECT`**: `*` e `t.*` expandem; o nome da coluna de saída é o alias, o nome da coluna ou o
   texto da expressão.
7. **`ORDER BY`**: um literal inteiro `n` é a posição do item; um nome que bate com o alias de um item
   usa a expressão dele; senão é uma expressão sobre as colunas de entrada.
8. **Montagem**: junções → `Aggregate` → `Filter` (`HAVING`) → `Sort` → `Project` → `Limit`.

### `base_scan` (escolha do índice)

1. Extrai restrições `coluna <op> constante` com `<op>` em `=, <, <=, >, >=` (o literal pode estar de
   qualquer lado; `const_value` avalia `-5`, `2*3` etc.); converte o literal para o **tipo da coluna**
   (`INT` → `FLOAT` ok; outros descasamentos são ignorados e o filtro resolve).
2. Para cada índice da tabela com restrições na sua coluna, calcula uma **pontuação**:

| Situação | Pontos |
|---|---:|
| igualdade, índice **único** | 100 |
| igualdade, índice não único | 80 |
| intervalo fechado (`lo` e `hi`) | 60 |
| intervalo aberto de um lado | 40 |

3. Escolhe a maior; sem índice aplicável → `SeqScan`.
4. Constrói os limites em bytes (veja [`keyenc`](keyenc.md)): igualdade `[k, prefix_end(k))`; `>` começa em
   `prefix_end(k)`, `>=` em `k`; `<` termina em `k`, `<=` em `prefix_end(k)`; sem limite inferior começa em
   `AFTER_NULL`.
5. **O predicado inteiro é reaplicado** num `Filter` por cima: o índice só *estreita*, nunca decide sozinho.

### `explain`
Uma linha por nó, com 2 espaços de indentação por nível; os filhos vêm depois do pai.

## 5. Invariantes

1. Todo plano gerado produz **o mesmo resultado** que um `SeqScan` + `Filter` com o `WHERE` completo.
2. Nenhum predicado do `WHERE` é empurrado para o lado direito de um `LEFT JOIN`.
3. Nenhuma `Expr::Agg` sobrevive dentro de um nó que será ligado com `bind` (todas viram `#aggN`).

## 6. Concorrência

O planejamento acontece com os locks das tabelas **já pegos** (`Inner::select`), então o conjunto de
índices lido por `TableInfo::indexes()` não muda durante o planejamento.

## 7. Erros

`Error::Exec`: tabela inexistente, alias repetido, coluna desconhecida ou ambígua, agregação em
`WHERE`/`ON`, `ON` referenciando tabela ainda não ligada, `t.*` sem tabela correspondente.

## 8. Testes (`tests/sql.rs`)

| Teste | Garante |
|---|---|
| `secondary_index_is_chosen_and_stays_consistent` | índice por igualdade e intervalos; mesmo resultado com e sem índice; `EXPLAIN` mostra a condição |
| `primary_key_uses_the_index_even_inside_and` | índice dentro de `AND`; filtro reaplicado |
| `joins_choose_the_right_algorithm_and_compute_correctly` | `IndexNestedLoopJoin`, `HashJoin`, `NestedLoopJoin`, `LEFT JOIN`, pushdown, ambiguidade |
| `aggregates_group_by_having_order_limit` | extração de agregações, `HAVING`, `ORDER BY` por alias e posição |

## 9. Limitações e evolução (D8)

* **Sem custo**: a ordem das junções é a do `FROM`; usa índice mesmo quando uma varredura seria mais barata.
* Sem *merge join*, sem *covering index*, sem aproveitar a ordem do índice para `ORDER BY`.
* Uma restrição por coluna: se há duas do mesmo lado (`id > 5 AND id > 10`), usa só a primeira (o filtro
  corrige o resto).
* `IN (...)` vira `OR`: não usa índice.

## 10. Veja também

[ADR-0015](../adr/0015-planejador-por-regras-e-executor-volcano.md), [`exec`](exec.md), [`keyenc`](keyenc.md),
[`../guides/query-tuning.md`](../guides/query-tuning.md)
