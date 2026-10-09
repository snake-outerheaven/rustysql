# ADR-0007: Chaves de índice *memcomparable*

* **Status**: aceita
* **Data**: 2026-10-09
* **Módulos afetados**: `keyenc`, `btree`, `catalog`, `plan`, `exec`

## Contexto e declaração do problema

A B-tree precisa comparar chaves para ordenar e buscar. Os valores SQL são de tipos diferentes (`INT`,
`FLOAT`, `TEXT`, `BOOL`, `NULL`) e a árvore, que é só uma estrutura de páginas, não deveria conhecer
tipos nem carregar um comparador por coluna. Além disso, um índice **não único** precisa distinguir
entradas com o mesmo valor, e consultas por intervalo precisam de limites calculáveis.

## Fatores de decisão

* a B-tree opera só sobre bytes (nós simples de serializar, comparação rápida);
* ordem da codificação = ordem SQL do valor (inclusive negativos e `FLOAT`);
* índice não único e busca por prefixo sem estruturas extras.

## Opções consideradas

1. **Comparador por tipo** passado à árvore; chaves guardadas como `Value`.
2. **Chaves como bytes comparáveis por `memcmp`** (*memcomparable*, como RocksDB e MySQL/InnoDB).

## Resultado da decisão

Escolhida a **opção 2**. `keyenc::encode_value`:

| Tipo | Codificação |
|---|---|
| `NULL` | `00` |
| `BOOL` | `01` + 1 byte |
| `INT` | `02` + `u64` big-endian com o **bit de sinal invertido** (negativos ficam antes dos positivos) |
| `FLOAT` | `03` + bits IEEE-754 big-endian: se o bit de sinal é 1, inverte tudo; senão, inverte só o bit de sinal |
| `TEXT` | `04` + bytes (`00` escapado como `00 FF`) + terminador `00 01`: **livre de prefixo** |

O tag de 1 byte separa os tipos. Para índices **não únicos** a chave é `valor ‖ page(u32 BE) ‖ slot(u16 BE)`,
o que torna cada entrada única e a mantém agrupada por valor. `prefix_end(k) = k ‖ FF×8` é maior que
qualquer entrada que comece com `k` (o sufixo de `RowId` tem 6 bytes). `AFTER_NULL = [01]` é o início
das buscas por intervalo, para não casar `NULL`s.

### Consequências

* **Bom**: a B-tree é agnóstica de tipos; nós guardam `Vec<u8>`.
* **Bom**: igualdade, `<`, `<=`, `>`, `>=`, `BETWEEN` e "igual a um valor num índice não único" viram
  intervalos de bytes `[start, end)` (veja `plan::base_scan`).
* **Bom**: `NULL`s ficam juntos no início e nunca entram numa busca por comparação.
* **Ruim**: uma chave codificada pode ser maior que o valor (o `TEXT` cresce com os `00`); o limite de
  512 bytes (`MAX_KEY`) vale para a forma codificada.
* **Ruim**: `INT` e `FLOAT` **não** são comparáveis entre si na forma codificada (tags diferentes);
  por isso o planejador converte o literal para o tipo da coluna e o hash join só usa a igualdade
  quando as duas colunas têm o mesmo tipo.
* **Ruim**: a ordem de `TEXT` é a dos **bytes UTF-8**, não a de nenhum idioma (sem *collation*).

## Validação

* `keyenc::tests::order_is_preserved`: sequências ordenadas de inteiros (de `i64::MIN` a `i64::MAX`),
  decimais, textos com `00` embutidos e tipos misturados mantêm a ordem após a codificação.
* `sql::secondary_index_is_chosen_and_stays_consistent`: intervalos abertos/fechados e
  `BETWEEN` devolvem as mesmas linhas com e sem índice.
* `btree::range_scan_is_ordered_and_bounded`.

## Links

* [`modules/keyenc.md`](../modules/keyenc.md), [`reference/storage-formats.md`](../reference/storage-formats.md#codificação-de-chaves-de-índice)
