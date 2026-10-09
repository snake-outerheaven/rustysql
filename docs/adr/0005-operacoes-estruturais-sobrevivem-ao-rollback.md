# ADR-0005: Operações estruturais sobrevivem ao rollback

* **Status**: aceita
* **Data**: 2026-10-09
* **Módulos afetados**: `txn`, `heap`, `wal`, `db`

## Contexto e declaração do problema

Quando um `INSERT` enche a última página do heap, o motor **aloca uma página nova e a encadeia**
(`HeapFile::insert` registra `HeapFormat` e `HeapLink`). Se essa transação sofrer *rollback*, as linhas
desaparecem, mas a página **continua alocada e encadeada na memória**: o rollback só desfaz o
conteúdo (undo), não a estrutura.

Como o WAL é gravado só no commit ([ADR-0003](0003-commit-atomico-um-registro.md)) e descartado no
rollback, surge um problema sutil:

1. T1 aloca a página P (ops `HeapFormat P`, `HeapLink A→P`) e dá rollback → as ops são descartadas;
2. T2 insere uma linha que cai em P e commita → o WAL de T2 tem `HeapInsert{page: P, ...}`;
3. queda. Na recuperação, P não foi formatada nem encadeada (as ops de T1 sumiram): a linha de T2
   fica numa página não alcançável pela varredura. **Dado commitado perdido.**

## Fatores de decisão

* não perder linhas commitadas, em nenhuma sequência de commits e rollbacks;
* manter o rollback simples e em memória ([ADR-0013](0013-rollback-em-memoria-e-atomicidade-por-comando.md));
* não introduzir um "desalocador de páginas" (a página nova pode já estar em uso por outra transação).

## Opções consideradas

1. **Desfazer a estrutura também** (desencadear e liberar a página no rollback).
2. **Tratar a estrutura como uma "ação de topo aninhada"**: as operações estruturais (`HeapFormat`,
   `HeapLink`) sobrevivem ao rollback e são gravadas no WAL por conta própria.
3. Logar as operações estruturais **imediatamente**, num registro à parte, na hora em que acontecem.

## Resultado da decisão

Escolhida a **opção 2**. `WalOp::is_structural()` marca `HeapFormat` e `HeapLink`.
`Txn::rollback_to` descarta as operações desfeitas **exceto** as estruturais, que ficam em `txn.wal`.
Em `Inner::rollback`, o que sobrou em `txn.wal` (só estruturais) é gravado no WAL como um registro de
commit próprio **antes** de soltar os locks (`release_all`). Assim, qualquer transação que depois
escreva naquelas páginas já encontra a estrutura no log, em ordem.

### Consequências

* **Bom**: o log sempre contém a estrutura antes das linhas que dependem dela.
* **Bom**: o rollback continua trivial; a página vazia fica disponível para a próxima transação.
* **Ruim**: um rollback pode gerar um registro de WAL (quando alocou páginas).
* **Ruim**: páginas alocadas por uma transação abortada que nunca são reaproveitadas ficam vazias
  (espaço desperdiçado, não corrupção).

## Prós e contras das opções

### 1. Desfazer a estrutura
* Contras: exige desalocar páginas e corrigir `next`; a página pode já ter sido usada por outra
  transação no meio-tempo (no caso de DDL concorrente ou páginas do catálogo): perigoso e complexo.

### 3. Logar na hora
* Contras: grava (e `fsync`a?) durante a transação, mesmo para as que abortam ou nunca commitam; quebra a
  regra "o WAL só tem commits" que simplifica a recuperação.

## Validação

`recovery::pages_allocated_by_a_rolled_back_transaction_still_work_after_recovery`: uma transação insere
400 linhas de ~200 bytes (≈20 páginas novas) e dá rollback; depois 400 linhas diferentes são commitadas
(caindo nas páginas deixadas); queda; recuperação; as 400 linhas estão lá.

**Teste de mutação.** Com o `rollback_to` alterado para descartar também as operações estruturais, o
teste falha: apenas **18 das 400** linhas commitadas sobrevivem à recuperação (as que couberam em
páginas que o log ainda conhecia). Ou seja, o bug descrito no contexto é real, não hipotético.

## Links

* [`modules/txn.md`](../modules/txn.md), [`modules/heap.md`](../modules/heap.md)
* Conceito: *nested top action* (ARIES, Mohan et al., 1992).
