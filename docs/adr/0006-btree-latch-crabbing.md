# ADR-0006: B+tree com *latch crabbing* e guards `Arc` (`parking_lot`)

* **Status**: aceita
* **Data**: 2026-10-09
* **Módulos afetados**: `btree`, `bufferpool`

## Contexto e declaração do problema

A B+tree é uma estrutura **hierárquica**: um nó interno aponta para filhos, e uma divisão de folha
altera o pai (e talvez o avô). Se duas threads mexem na árvore ao mesmo tempo sem disciplina, é
possível enxergar um nó no meio de uma divisão (chave que "some"), perder uma inserção ou corromper
ponteiros. É preciso garantir a **integridade do estado da árvore** com várias threads, sem serializar
tudo atrás de um lock global.

## Fatores de decisão

* integridade da árvore sob qualquer intercalação (requisito explícito);
* leitores concorrentes sem bloqueio entre si;
* escritores em subárvores diferentes não deveriam se esperar;
* ausência de deadlock de latch;
* Rust seguro (sem `unsafe` no projeto).

## Opções consideradas

1. **Um `RwLock` para a árvore inteira**: leitores compartilham, escritor exclusivo.
2. **Latch crabbing** (*lock coupling*): um latch por nó; descer pegando o do filho antes de soltar o
   do pai; escritor solta os ancestrais quando o filho é "seguro".
3. **B-link tree** (Lehman & Yao): ponteiros laterais permitem soltar o pai antes de pegar o filho.
4. Árvore **sem lock** (Bw-tree, OLC).

## Resultado da decisão

Escolhida a **opção 2**. Cada página do pool é um `Arc<Frame>` com um `RwLock` (o *latch*). Os guards
precisam ser **"owned"** (carregam o `Arc`), porque a técnica exige segurar o guard do pai e do filho ao
mesmo tempo e depois soltar só o do pai, o que um guard emprestado do `std` não permite sem `unsafe`.
Por isso a única dependência do projeto é o `parking_lot` (`arc_lock`: `read_arc`/`write_arc`).

Regras (`btree.rs`):

| Operação | Protocolo |
|---|---|
| `search` | latch compartilhado no filho, depois solta o do pai |
| `insert` (otimista) | desce com latches compartilhados; **exclusivo só na folha**; se cabe, insere |
| `insert` (pessimista) | quando a folha vai dividir: desce com latches **exclusivos**, solta todos os ancestrais ao encontrar um filho *seguro* (`size + MAX_ENTRY ≤ 4096`); divide de baixo para cima com os que sobraram |
| `delete` | desce compartilhado, exclusivo só na folha (sem fusão) |
| `scan` de intervalo | um latch por vez, seguindo `next` das folhas |

A ordem de aquisição é sempre **raiz → folha** e **esquerda → direita**: ciclo impossível.

### Consequências

* **Bom**: leitores nunca bloqueiam leitores; o caso comum de escrita (folha com espaço) só trava a folha.
* **Bom**: a integridade é verificável: `BTree::verify` confere ordem das chaves, limites do pai,
  níveis e a cadeia de folhas.
* **Bom**: sem `unsafe` no projeto.
* **Ruim**: durante um split pessimista a **raiz** fica travada em modo exclusivo se todos os nós do
  caminho estão cheios: uma escrita rara, mas que serializa a árvore por um instante.
* **Ruim**: dependência externa (`parking_lot`).
* **Ruim**: cada acesso decodifica o nó da página (`Node::decode`): simples, mas não é o mais rápido.

## Prós e contras das opções

### 1. Lock global da árvore
* Prós: trivial e obviamente correto. Contras: escritores serializam toda a árvore; não ensina o
  problema que se quer ensinar.

### 2. Latch crabbing (escolhida)
* Prós: protocolo clássico (Bayer & Schkolnick), correto e ensinável. Contras: acima.

### 3. B-link tree
* Prós: menos latches simultâneos. Contras: mais campos por nó, lógica de "mover para a direita" ao
  encontrar um nó dividido; ganho irrelevante na escala do projeto.

### 4. Lock-free
* Contras: complexidade e dificuldade de verificar; fora do escopo.

## Validação

Esta é a decisão com os testes mais fortes do projeto:

| Teste | O que prova |
|---|---|
| `btree::concurrent_writers_and_readers_preserve_integrity` | 8 escritores intercalados + 4 leitores (busca e varredura): nenhuma chave âncora some, varreduras sempre ordenadas, `verify()` ok no fim |
| `btree::concurrent_inserts_and_deletes_with_deep_tree` | 6 threads inserindo e apagando chaves de 150 bytes numa árvore de 3 níveis |
| `btree::random_inserts_search_delete_keep_invariants` | os mesmos invariantes, sem concorrência |

**Teste de mutação.** Para garantir que o teste de concorrência realmente detecta o bug que ele
promete detectar, o protocolo foi quebrado de propósito (soltar o latch do pai **antes** de pegar o do
filho na leitura). Com a versão inicial do teste a mutação **sobreviveu** (as chaves dos leitores
ficavam num canto da árvore que nenhuma divisão tocava); o teste foi reforçado com chaves âncora
intercaladas com as dos escritores, e então passou a falhar com a mutação. O código correto passou em
25 de 25 execuções em modo *release*.

## Links

* Bayer & Schkolnick, *Concurrency of Operations on B-Trees* (1977).
* [`modules/btree.md`](../modules/btree.md), [`modules/bufferpool.md`](../modules/bufferpool.md),
  [`architecture/runtime-scenarios.md`](../architecture/runtime-scenarios.md#6-divisão-de-um-nó-da-b-tree-latch-crabbing)
