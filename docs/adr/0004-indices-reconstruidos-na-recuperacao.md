# ADR-0004: Índices não são logados; reconstruídos na recuperação

* **Status**: aceita
* **Data**: 2026-10-09
* **Módulos afetados**: `wal`, `btree`, `db`

## Contexto e declaração do problema

O WAL de redo ([ADR-0002](0002-no-steal-no-force-wal-redo.md)) reaplica operações sobre páginas. Para
o heap isso é simples: `inserir no slot X da página P`. Para a B-tree seria preciso logar também as
divisões de nós, as páginas novas, os ponteiros dos pais... e, ao reaplicar, reproduzir exatamente a
mesma estrutura. O problema: a estrutura de uma B-tree depende da **ordem** em que as threads
inseriram as chaves, e essa ordem não é a ordem do log.

## Fatores de decisão

* corretude da recuperação acima de tudo;
* um índice é **dado derivado**: pode ser recalculado a partir do heap;
* não complicar o formato do WAL.

## Opções consideradas

1. **Logar as operações de índice** (inserir/apagar chave) e reaplicá-las na recuperação.
2. **Logar páginas de índice** (imagens físicas após cada mudança).
3. **Não logar** índices; **reconstruí-los** a partir do heap sempre que a recuperação reaplicou algo.

## Resultado da decisão

Escolhida a **opção 3**. O WAL só tem operações de heap (`WalOp::Heap*`). Em `Database::open_with`,
se o WAL tinha registros, `Inner::rebuild_indexes` zera cada árvore (`BTree::reset`: a raiz vira uma
folha vazia) e reinsere todas as linhas lidas do heap. Em seguida o checkpoint grava o resultado.

Quando **não** houve recuperação (o WAL estava vazio), os índices vêm direto do arquivo, íntegros,
porque o último checkpoint gravou heap e índices juntos.

### Consequências

* **Bom**: o formato do WAL tem 4 tipos de operação; nenhuma depende de estrutura de árvore.
* **Bom**: a recuperação *conserta* qualquer inconsistência de índice, por construção.
* **Bom**: nada a reaplicar de forma idempotente na B-tree.
* **Ruim**: o custo da recuperação é **O(dados)**: lê todas as tabelas que têm índice (dívida D5).
* **Ruim**: as páginas antigas dos índices ficam órfãs (a raiz é reaproveitada, os filhos não).
* **Ruim**: a unicidade de uma chave primária é verificada de novo na reconstrução; dados já
  inconsistentes fariam a abertura falhar com `Error::Storage("chave duplicada ao reconstruir ...")`.

## Prós e contras das opções

### 1. Logar operações de índice
* Prós: recuperação rápida. Contras: a reaplicação precisa reproduzir a mesma ordem de inserção
  entre threads; qualquer divergência muda a estrutura (e os ids de página que o catálogo guarda).

### 2. Logar páginas de índice
* Prós: determinístico. Contras: volume de log enorme (uma página de 4 KB por inserção).

### 3. Reconstruir (escolhida)
* Prós/contras: acima.

## Validação

* `recovery::committed_work_survives_a_crash_including_indexes`: após a queda, o `EXPLAIN` usa o índice
  e a busca por ele devolve as linhas certas (inclusive sem a linha apagada), e a PK continua valendo.
* `recovery::ddl_survives_a_crash`: índices criados e removidos antes da queda aparecem como esperado.

## Links

* [`modules/btree.md`](../modules/btree.md), [`modules/db.md`](../modules/db.md)
