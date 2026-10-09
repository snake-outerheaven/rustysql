# ADR-0008: Raiz fixa e `delete` sem fusão de nós

* **Status**: aceita (com dívida técnica D3)
* **Data**: 2026-10-09
* **Módulos afetados**: `btree`, `catalog`

## Contexto e declaração do problema

Duas escolhas de projeto da B-tree que parecem independentes mas têm a mesma raiz: reduzir o que é
preciso coordenar entre threads e entre a árvore e o resto do sistema.

1. **A raiz muda de lugar quando divide?** Numa B-tree clássica, quando a raiz estoura, cria-se uma
   *nova raiz* acima dela. Isso muda o "ponteiro para a raiz", que o catálogo guarda (precisa ser
   atualizado, de forma durável e concorrente).
2. **O que fazer quando um nó fica com poucas chaves após remoções?** Uma B-tree clássica funde ou
   redistribui entre irmãos para manter ocupação mínima.

## Fatores de decisão

* o id da raiz fica gravado no catálogo (`IndexInfo.tree`, linha `("index", ..., raiz)`); mudá-lo seria
  uma atualização transacional do catálogo no meio de uma inserção;
* a fusão de nós obriga a travar **irmãos** (além de pai e filho) e a lidar com ponteiros `next` de
  folhas, o que complica o protocolo de latches ([ADR-0006](0006-btree-latch-crabbing.md));
* varreduras de intervalo seguem `next` sem latch de acoplamento: páginas **nunca podem sumir** enquanto
  alguém as visita.

## Opções consideradas

**Raiz**: (a) a raiz muda de id; (b) a raiz **mantém o id** e, ao dividir, o conteúdo antigo é copiado
para uma página nova.

**Remoção**: (a) fusão/redistribuição; (b) só remover da folha, deixando-a possivelmente vazia.

## Resultado da decisão

* **Raiz fixa (b)**: ao dividir, `BTree::insert_pessimistic` aloca duas páginas novas (esquerda e
  direita), escreve as metades nelas e reescreve a **própria raiz** como um nó interno de dois filhos. O
  id da raiz nunca muda, então o catálogo nunca precisa ser atualizado.
* **Sem fusão (b)**: `BTree::delete` remove a chave da folha e para. Folhas vazias continuam na
  cadeia de `next` e podem receber chaves de novo.

### Consequências

* **Bom**: nenhuma atualização de catálogo durante inserções; recuperação só precisa zerar a raiz
  (`BTree::reset`) para reconstruir ([ADR-0004](0004-indices-reconstruidos-na-recuperacao.md)).
* **Bom**: nenhuma página da árvore é liberada, então ponteiros `next` e varreduras nunca ficam
  pendurados; o `delete` só pega latch na folha.
* **Bom**: protocolo de latches mais simples e fácil de verificar.
* **Ruim (D3)**: depois de muitas remoções a árvore fica **esparsa**: páginas quase vazias continuam
  alocadas, e varreduras visitam folhas vazias. A altura nunca diminui.
* **Ruim**: a cópia do conteúdo da raiz numa divisão custa uma página extra por aumento de altura.

## Validação

* `btree::random_inserts_search_delete_keep_invariants`: remove metade das chaves e `verify()` ainda passa
  (ordem, limites, níveis, cadeia de folhas).
* `btree::concurrent_inserts_and_deletes_with_deep_tree`.

## Evolução possível

Fusão de folhas com latch no irmão da esquerda e *tombstones* de página (para varreduras em curso),
ou uma lista de páginas livres compartilhada com o heap (que resolveria também D4).

## Links

* [`modules/btree.md`](../modules/btree.md)
