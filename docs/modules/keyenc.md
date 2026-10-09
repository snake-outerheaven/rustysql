# Módulo `keyenc`

> Codificação de chaves *memcomparable*: comparar bytes (`memcmp`) é comparar valores.

| | |
|---|---|
| **Arquivo** | `src/keyenc.rs` |
| **Camada** | armazenamento |
| **Depende de** | `heap` (tipo `RowId`), `value` |
| **Usado por** | `catalog` (chaves de índice), `plan` (limites de intervalo), `exec` (chave do hash join e da busca por índice), `db` |
| **Testes** | unitário `keyenc::tests::order_is_preserved`; `btree::range_scan_is_ordered_and_bounded`; `sql::secondary_index_is_chosen_and_stays_consistent` |

## 1. Responsabilidade

Transformar um `Value` em bytes cuja **ordem lexicográfica** é a ordem SQL do valor, para que a
[`btree`](btree.md) trabalhe só com `Vec<u8>`, sem conhecer tipos. Justificativa:
[ADR-0007](../adr/0007-chaves-memcomparable.md).

## 2. API pública

| Item | Descrição |
|---|---|
| `encode_value(&Value, &mut Vec<u8>)` | acrescenta a codificação |
| `value_key(&Value) -> Vec<u8>` | codificação de um valor (chave de índice **único**) |
| `value_rid_key(&Value, RowId) -> Vec<u8>` | `valor ‖ page(u32 BE) ‖ slot(u16 BE)` (índice **não único**) |
| `prefix_end(&[u8]) -> Vec<u8>` | `chave ‖ FF×8`: maior que qualquer chave com esse prefixo |
| `AFTER_NULL: [u8; 1]` | `[0x01]`: primeira chave que não é `NULL` |

## 3. Estruturas de dados

| Valor | Bytes |
|---|---|
| `NULL` | `00` |
| `BOOL` | `01` ‖ `00`/`01` |
| `INT` | `02` ‖ `(x as u64) ^ 0x8000…` em big-endian (inverte o bit de sinal) |
| `FLOAT` | `03` ‖ `bits` big-endian; se o bit de sinal é 1 → `!bits`, senão `bits | 1<<63` |
| `TEXT` | `04` ‖ bytes UTF-8 (cada `00` vira `00 FF`) ‖ `00 01` |

Propriedades:

* **Ordem entre tipos**: `NULL < BOOL < números < TEXT` (pelo tag). `INT` e `FLOAT` têm tags
  diferentes, então **não são comparáveis entre si** na forma codificada (o planejador converte o
  literal para o tipo da coluna).
* **Livre de prefixo**: nenhuma codificação de `TEXT` é prefixo de outra (o terminador `00 01` e o
  escape `00 FF` garantem), o que dá consistência a `valor ‖ rid`.
* **`FLOAT`**: ordem total, com `-0.0 < +0.0` e `NaN` fora da ordem numérica (`NaN`s positivos acima de
  `+∞`).

## 4. Algoritmos e fluxos

Sem laços interessantes. Uso típico num índice não único:

```
entrada:  value_rid_key(v, rid)               → chave guardada na B-tree
busca v:  [ value_key(v), prefix_end(value_key(v)) )   → todas as entradas de valor v
x > v:    [ prefix_end(value_key(v)), … )
x >= v:   [ value_key(v), … )
x < v:    [ AFTER_NULL, value_key(v) )
x <= v:   [ AFTER_NULL, prefix_end(value_key(v)) )
```

Para um índice **único** a chave é só `value_key(v)` (a B-tree rejeita duplicatas); `NULL` num índice
único usa a forma com `RowId` (vários `NULL`s não violam unicidade).

## 5. Invariantes

* `a < b` (SQL, mesmo tipo) ⇔ `encode(a) < encode(b)` (comparação de bytes).
* `prefix_end(k) > e` para toda entrada `e` que começa com `k` (o sufixo de `RowId` tem 6 bytes < 8).

## 6. Concorrência

Funções puras: sem estado.

## 7. Erros

Nenhum: não falha.

## 8. Testes

`keyenc::tests::order_is_preserved`: inteiros de `i64::MIN` a `i64::MAX`, decimais de `-1e300` a `1e300`,
textos incluindo `\0` embutidos (`"a" < "a\0" < "a\0b" < "ab" < "b"`) e tipos misturados.

## 9. Limitações e evolução

* Chaves de uma única coluna (um índice composto exigiria concatenar as codificações, o que a
  propriedade livre de prefixo já permite).
* `TEXT` sem *collation*: a ordem é a dos bytes UTF-8.
* O tamanho codificado de um texto pode passar do original (cada `00` vira dois bytes).

## 10. Veja também

[ADR-0007](../adr/0007-chaves-memcomparable.md), [`btree`](btree.md), [`plan`](plan.md), [`../reference/storage-formats.md`](../reference/storage-formats.md#codificação-de-chaves-de-índice)
