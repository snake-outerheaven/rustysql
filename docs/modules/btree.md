# Módulo `btree`

> B+tree em disco, segura para várias threads, com *latch crabbing*. É o índice.

| | |
|---|---|
| **Arquivo** | `src/btree.rs` |
| **Camada** | armazenamento |
| **Depende de** | `bufferpool`, `disk`, `error`, `heap` (tipo `RowId`) |
| **Usado por** | `catalog` (`IndexInfo.tree`), `txn` (undo), `exec` (scans e joins), `db` |
| **Testes** | `tests/btree.rs` (5 testes) + índices nos testes de SQL e recuperação |

## 1. Responsabilidade

Mapear **chaves em bytes → `RowId`**, em ordem, com busca, inserção, remoção e varredura de intervalo,
mantendo a estrutura **íntegra sob qualquer intercalação de threads**.

**Não faz**: interpretar chaves (veja [`keyenc`](keyenc.md)), garantir unicidade lógica entre transações
(isso é do [`lockmgr`](lockmgr.md); a árvore só recusa chave **idêntica**), nem logar suas operações no WAL
([ADR-0004](../adr/0004-indices-reconstruidos-na-recuperacao.md)).

## 2. API pública

| Item | Descrição |
|---|---|
| `BTree::create(&pool)` | árvore vazia (uma folha) numa página nova |
| `BTree::open(pool, root)` | abre a partir da raiz guardada no catálogo |
| `root()` | id da página raiz (**nunca muda**) |
| `search(key) -> Option<RowId>` | busca exata |
| `insert(key, rid) -> bool` | `false` se a chave já existe |
| `delete(key) -> bool` | `true` se existia |
| `scan(start, end) -> BTreeScan` | `start <= chave < end` (`end = None`: até o fim), em ordem |
| `verify() -> TreeStats` / `stats()` | confere **todos** os invariantes; devolve chaves, altura, folhas, nós |
| `reset()` | volta a raiz a uma folha vazia (reconstrução de índices) |
| `MAX_KEY = 512` | tamanho máximo de chave codificada |

`BTree: Clone` barato (um `Arc<BufferPool>` e um `u32`); várias threads usam cópias.

## 3. Estruturas de dados

```rust
enum Node {
    Leaf     { keys: Vec<Vec<u8>>, rids: Vec<RowId>, next: PageId },
    Internal { level: u8, keys: Vec<Vec<u8>>, children: Vec<PageId> },  // children.len() == keys.len() + 1
}
```

* Folha: `keys[i] → rids[i]`; `next` encadeia as folhas (varredura de intervalo).
* Interno: `children[i]` contém chaves `< keys[i]`; `children[i+1]`, chaves `>= keys[i]`.
* `level`: 0 nas folhas; um nó interno tem o nível dos filhos + 1.
* Formato em página (cabeçalho de 8 bytes + entradas) em
  [`../reference/storage-formats.md`](../reference/storage-formats.md#página-de-b-tree).
* Cada nó é **decodificado** da página para `Node` e **recodificado** ao gravar (simples; não é o mais
  rápido).

## 4. Algoritmos e fluxos

### Busca e varredura
`find_leaf_read`: pega o latch compartilhado da raiz e, a cada nível, **pega o latch do filho antes de
soltar o do pai**. `search` procura na folha com `binary_search`. `scan` começa na folha que cobre
`start` e depois segue `next`, **um latch por vez**: lê a folha inteira para um buffer, solta o latch,
e só então busca a seguinte (a leitura da folha seguinte nunca perde nem repete uma chave que já
existia quando a varredura começou, mesmo com divisões em curso).

### Inserção
1. **Otimista** (`insert`): `find_leaf_write` desce com latches compartilhados e pega o **exclusivo só na
   folha** (se a raiz é folha, troca o latch e revalida). Se a chave cabe (`tamanho + entrada ≤ 4096`),
   insere e termina. Este é o caso comum.
2. **Pessimista** (`insert_pessimistic`), se a folha vai dividir: desce com latches **exclusivos**
   guardando o caminho; ao encontrar um filho **seguro** (`tamanho + MAX_ENTRY ≤ 4096`, ou seja, absorve
   mais uma entrada sem dividir), **solta todos os ancestrais**. Insere na folha e, enquanto um nó
   estoura, divide-o (`Node::split`, por **bytes**, não por contagem) e insere o separador no pai.
   Se a **raiz** estoura: a metade esquerda vai para uma página nova, a direita para outra, e a raiz
   é reescrita como nó interno de dois filhos (o id não muda).

### Remoção
`find_leaf_write` e remove da folha. **Sem fusão de nós** ([ADR-0008](../adr/0008-raiz-fixa-e-delete-sem-fusao.md)).

### `verify`
Percorre a árvore conferindo: chaves estritamente crescentes em cada nó; chaves de um filho dentro dos
limites que o pai impõe; nível do filho = nível do pai − 1; nº de filhos = nº de chaves + 1; e que a
cadeia `next` das folhas é exatamente a ordem da travessia.

## 5. Invariantes

1. Em todo nó, as chaves são estritamente crescentes.
2. `children[i]` só tem chaves `< keys[i]`; `children[i+1]`, `>= keys[i]` (limites herdados do pai).
3. Todos os nós folha estão no mesmo nível; a cadeia `next` percorre todas as folhas em ordem de chave.
4. A raiz mantém o id; `next` da última folha é `0`.
5. Todo nó cabe em 4096 bytes depois de cada operação.

## 6. Concorrência

Protocolo completo em [ADR-0006](../adr/0006-btree-latch-crabbing.md). Resumo:

| Operação | Latches |
|---|---|
| `search` | compartilhado, encadeado, raiz → folha |
| `insert` otimista | compartilhado até a folha; exclusivo só nela |
| `insert` pessimista | exclusivo, raiz → folha; ancestrais soltos ao achar um filho seguro |
| `delete` | compartilhado até a folha; exclusivo só nela |
| `scan` | um compartilhado por vez, esquerda → direita |

* A ordem é sempre **raiz → folha** e **esquerda → direita**: sem ciclo, sem deadlock de latch.
* Os guards são `Arc` (`PageWriteGuard`), por isso o caminho pode ser guardado num `Vec` e os
  ancestrais soltos fora de ordem de escopo (`path.clear()`).
* Durante o split, o escritor segura o nó pai até terminar de reescrever o filho: nenhum leitor vê um
  separador sem o filho correspondente.

## 7. Erros

`Error::Exec` para chave vazia ou maior que `MAX_KEY`; `Error::Storage` para nó corrompido ou pool
cheio; `insert` devolve `Ok(false)` (não erro) para chave duplicada.

## 8. Testes

| Teste | O que prova |
|---|---|
| `random_inserts_search_delete_keep_invariants` | 6000 chaves embaralhadas; `verify` em passos; remoção da metade |
| `range_scan_is_ordered_and_bounded` | limites inclusivo/exclusivo; atravessa folhas; início entre chaves |
| `long_text_keys_force_deep_tree` | chaves de ~300 B → 3 níveis; chave de 600 B e chave vazia recusadas |
| `concurrent_writers_and_readers_preserve_integrity` | 8 escritores intercalados + 4 leitores (busca e varredura); âncoras nunca somem; varredura ordenada; `verify` |
| `concurrent_inserts_and_deletes_with_deep_tree` | 6 threads inserindo/apagando chaves de 150 B |

Este conjunto foi validado por **mutação**: remover o acoplamento de latches na leitura faz o teste
concorrente falhar; o código correto passou 25 de 25 execuções em *release*.

## 9. Limitações e evolução

* Sem fusão de nós (D3): a árvore pode ficar esparsa.
* Cada acesso decodifica a página inteira (poderia operar sobre os bytes).
* Chave de 1 coluna, até 512 bytes codificados.
* Um split pessimista com todos os nós cheios serializa a árvore por um instante.

## 10. Veja também

[ADR-0006](../adr/0006-btree-latch-crabbing.md), [ADR-0007](../adr/0007-chaves-memcomparable.md),
[ADR-0008](../adr/0008-raiz-fixa-e-delete-sem-fusao.md), [`bufferpool`](bufferpool.md), [`keyenc`](keyenc.md),
[cenário de split](../architecture/runtime-scenarios.md#6-divisão-de-um-nó-da-b-tree-latch-crabbing)
