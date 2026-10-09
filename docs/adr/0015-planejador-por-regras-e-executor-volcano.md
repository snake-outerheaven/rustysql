# ADR-0015: Planejador por regras e executor *Volcano*

* **Status**: aceita
* **Data**: 2026-10-09
* **Módulos afetados**: `plan`, `exec`, `eval`, `catalog`

## Contexto e declaração do problema

Depois do parser há uma árvore sintática. É preciso transformá-la em execução: que índices usar, em que
ordem e por qual algoritmo juntar tabelas, onde aplicar cada filtro, como agregar. E é preciso mostrar
isso ao usuário (`EXPLAIN`).

## Fatores de decisão

* resultados corretos em primeiro lugar (um índice nunca pode mudar o resultado);
* escolhas **explicáveis** e determinísticas (didático);
* sem estatísticas de dados (não há coleta de cardinalidades);
* execução em fluxo sempre que possível (memória).

## Opções consideradas

**Planejamento**: (a) interpretar a árvore sintática direto, sem plano; (b) **otimizador por regras**
(heurísticas); (c) otimizador por **custo** com estatísticas.

**Execução**: (a) cada operador produz um `Vec` completo; (b) modelo **Volcano** (iteradores
`next()` encadeados, uma linha por vez).

## Resultado da decisão

**Planejador por regras (b)** em `plan.rs` e executor **Volcano (b)** em `exec.rs`.

### Regras do planejador

1. **Pushdown**: conjuntos do `WHERE` que referenciam **uma só tabela** descem até o scan dessa tabela;
   conjuntos do `ON` que citam só a tabela da direita também. Exceção: nunca se empurra um predicado
   do `WHERE` para o lado direito de um `LEFT JOIN` (mudaria o resultado).
2. **Caminho de acesso** (`base_scan`): extrai restrições `coluna <op> constante` (`=`, `<`, `<=`, `>`,
   `>=`), converte o literal para o tipo da coluna e escolhe o melhor índice por pontuação:
   igualdade em índice **único** (100) > igualdade (80) > intervalo fechado dos dois lados (60) >
   intervalo de um lado (40) > varredura completa. O predicado é **sempre reaplicado** como `Filter`
   por cima do `IndexScan`: o índice só estreita o conjunto, nunca decide sozinho.
3. **Junção**, para cada `JOIN` na ordem do `FROM` (associação à esquerda), a partir de uma igualdade
   `esquerda = direita`: **index nested loop** se a coluna da direita tem índice; senão **hash join**;
   senão **nested loop**. A igualdade só vale se, quando ambos os lados são colunas simples, os tipos
   são iguais (as chaves codificadas de `INT` e `FLOAT` não casam).
4. **Agregação**: as chamadas (`SUM(x)`) são extraídas para colunas sintéticas `#aggN`; o resto da
   consulta (itens, `HAVING`, `ORDER BY`) passa a referenciá-las.
5. Ordem dos operadores: junções → `Aggregate` → `Filter` (`HAVING`) → `Sort` → `Project` → `Limit`.

### Executor

Cada `Node` vira um iterador (`RowIter`). `Sort`, `Aggregate` e o lado de construção do `HashJoin`
**materializam** a entrada; os demais operam em fluxo. `UPDATE` e `DELETE` usam o mesmo planejamento
de um scan, mas **materializam** as linhas casadas antes de modificar qualquer coisa (evita ler o que
o próprio comando acabou de escrever).

### Consequências

* **Bom**: `EXPLAIN` mostra exatamente o que será executado, com o índice escolhido e as condições.
* **Bom**: nenhuma estatística a manter; planos previsíveis.
* **Bom**: o `Filter` reaplicado torna o índice **seguro por construção**; um bug de bounds
  produz resultado errado só se o índice devolver *menos* linhas do que o necessário, e os testes
  comparam resultados com e sem índice.
* **Ruim (D8)**: **ordem de junção = ordem do `FROM`**; sem reordenação por custo, uma consulta mal
  escrita pode ser lenta. Escolhe índice mesmo quando uma varredura seria mais barata (tabela pequena
  ou intervalo quase total).
* **Ruim**: sem *join* por intercalação (merge), sem *covering index*, sem aproveitar a ordem do
  índice para `ORDER BY`.
* **Ruim**: `Sort` e `Aggregate` precisam caber na memória.

## Prós e contras das opções

### Por custo
* Prós: melhores planos. Contras: precisa de estatísticas atualizadas, modelo de custo e
  enumeração de ordens de junção: um projeto à parte.

### Interpretar direto
* Contras: sem índices nem algoritmos de junção, tudo seria laço aninhado sobre varreduras.

## Validação (`tests/sql.rs`)

| Teste | Garante |
|---|---|
| `secondary_index_is_chosen_and_stays_consistent` | escolha do índice (igualdade, intervalos, `BETWEEN`, literal à esquerda); resultados iguais com e sem índice |
| `primary_key_uses_the_index_even_inside_and` | índice dentro de `AND` + filtro reaplicado; `UPDATE`/`DELETE` também planejam por índice |
| `joins_choose_the_right_algorithm_and_compute_correctly` | `IndexNestedLoopJoin`, `HashJoin`, `NestedLoopJoin`; `LEFT JOIN`; pushdown; ambiguidade |
| `aggregates_group_by_having_order_limit` | agregação, `GROUP BY`, `HAVING`, `ORDER BY` por alias/posição, `LIMIT/OFFSET` |

## Links

* Graefe, *Volcano: an extensible and parallel query evaluation system* (1994).
* [`modules/plan.md`](../modules/plan.md), [`modules/exec.md`](../modules/exec.md),
  [`guides/query-tuning.md`](../guides/query-tuning.md)
