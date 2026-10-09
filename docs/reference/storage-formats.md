# Formatos de armazenamento

Especificação byte a byte dos dois arquivos que o rustysql grava, com **exemplos reais** extraídos de
um banco de verdade (`cargo run --example storage_dump` reproduz tudo o que está abaixo).

**Convenções.** Inteiros multi-byte são **little-endian**, exceto nas *chaves de índice*
([seção própria](#codificação-de-chaves-de-índice)), que são **big-endian** para que a ordem dos bytes
seja a ordem dos valores. `u8/u16/u32/u64` = inteiros sem sinal de 1/2/4/8 bytes; `i64` = com sinal.
Deslocamentos em decimal, salvo `0x`.

## Visão geral

| Arquivo | Conteúdo | Quando é escrito |
|---|---|---|
| `<nome>.db` | vetor de páginas de 4096 bytes | **só** em checkpoint (e na criação do arquivo) |
| `<nome>.db.wal` | sequência de registros de commit | a cada commit |

```
banco.db
┌────────────┬────────────┬────────────┬────────────┬─────
│ página 0   │ página 1   │ página 2   │ página 3   │ ...
│ cabeçalho  │ raiz do    │ (heap ou   │ (heap ou   │
│ (magic)    │ catálogo   │  B-tree)   │  B-tree)   │
└────────────┴────────────┴────────────┴────────────┴─────
   4096 B        4096 B       4096 B       4096 B
```

---

## Arquivo de dados

* **Página `n`** ocupa os bytes `n × 4096 .. (n+1) × 4096`.
* **Número de páginas** = `ceil(tamanho_do_arquivo / 4096)` (não é guardado no cabeçalho).
* **Página 0**: cabeçalho. Só os 8 primeiros bytes importam: o *magic* `RUSTYSQ2` (`52 55 53 54 59 53
  51 32`); o resto é zero. Outro valor (inclusive o formato antigo `RUSTYSQL`) é recusado.
* **Página 1**: raiz do **heap do catálogo** (fixa).
* **Demais páginas**: páginas de heap (uma cadeia por tabela) ou nós de B-tree, alocadas em ordem de
  criação. Nada no arquivo diz de que tipo é uma página: quem a usa sabe (o catálogo guarda as raízes).
* Páginas nunca são recicladas: o arquivo só cresce.

Exemplo real (primeiros 16 bytes do arquivo):

```
0000  52 55 53 54 59 53 51 32 00 00 00 00 00 00 00 00  |RUSTYSQ2........|
```

---

## Página de heap

Usada pelas tabelas e pelo catálogo. Layout:

```
 0        4      6      8                                    free_end            4096
 ┌────────┬──────┬──────┬───────────────────────────────┬ ─ ─ ─ ─ ┬──────────────────┐
 │  next  │nslots│free_ │ slot 0 │ slot 1 │ ... │slot n-1│  livre  │ ... reg 1 │ reg 0 │
 │  u32   │ u16  │ end  │ 4 bytes cada                  │         │ crescem p/ a esq. │
 │        │      │ u16  │                               │         │                   │
 └────────┴──────┴──────┴───────────────────────────────┴ ─ ─ ─ ─ ┴──────────────────┘
```

| Bytes | Campo | Significado |
|---|---|---|
| 0..4 | `next` | próxima página da tabela; `0` = fim da cadeia |
| 4..6 | `nslots` | quantidade de slots (vivos + livres) |
| 6..8 | `free_end` | offset onde começa o último registro gravado (4096 numa página vazia) |
| 8 + 4·i | slot `i` | `offset: u16`, `len: u16`; **`offset = 0` = slot livre** |

* Registros crescem do **fim** da página para o começo; o vetor de slots, do começo para o fim.
* Um registro tem no máximo `4096 − 8 − 4 = 4084` bytes (`MAX_RECORD`).
* **Compactação**: quando há espaço total mas não contíguo, os registros vivos são reescritos colados no
  fim; os índices de slot **não** mudam.
* O endereço de uma linha é o `RowId = (página, slot)`.

### Exemplo real: a tabela `t (id INT PRIMARY KEY, nome TEXT)` com duas linhas

Cabeçalho e fim da página 2:

```
0000  00 00 00 00 02 00 de 0f ef 0f 11 00 de 0f 11 00  |................|
0010  00 00 00 00 00 00 00 00                          |........|
...
0fe0  00 00 00 00 00 00 00 03 03 00 00 00 62 69 61 01  |............bia.|
0ff0  01 00 00 00 00 00 00 00 03 03 00 00 00 61 6e 61  |.............ana|
```

Decodificação:

| Bytes | Valor | Leitura |
|---|---|---|
| `00 00 00 00` | `next = 0` | única página da tabela |
| `02 00` | `nslots = 2` | duas linhas |
| `de 0f` | `free_end = 0x0fde = 4062` | o registro mais baixo começa em 4062 |
| `ef 0f` `11 00` | slot 0: offset `0x0fef = 4079`, len `0x11 = 17` | linha `(1,'ana')` |
| `de 0f` `11 00` | slot 1: offset `0x0fde = 4062`, len 17 | linha `(2,'bia')` |

Repare que a **linha inserida primeiro** ('ana', slot 0) fica **no fim** da página (offset maior).

---

## Linha (registro do heap)

Uma linha é a concatenação dos seus valores, cada um com **1 byte de tag**:

| Tag | Valor | Conteúdo depois da tag |
|---:|---|---|
| `00` | `NULL` | nada |
| `01` | `INT` | 8 bytes (`i64`, little-endian) |
| `02` | `FLOAT` | 8 bytes (IEEE-754, little-endian) |
| `03` | `TEXT` | `u32` com o tamanho + bytes UTF-8 |
| `04` | `BOOL` | 1 byte (`00`/`01`) |

A linha **não** guarda o número de colunas nem os tipos: termina quando os bytes acabam, e quem lê conhece
o esquema.

Exemplo: o registro de `(1, 'ana')` (17 bytes, os últimos do exemplo acima):

```
01 01 00 00 00 00 00 00 00    03 03 00 00 00 61 6e 61
└─┬┘ └──────────┬─────────┘    └┬┘ └───┬───┘ └──┬──┘
 tag INT     1 (i64 LE)      tag  tamanho=3   "ana"
                              TEXT
```

---

## Catálogo

O heap da página 1 guarda **uma linha por tabela e uma por índice**, no formato de linha acima:

```
("table", nome: TEXT, raiz_do_heap: INT, esquema: TEXT)
("index", nome: TEXT, tabela: TEXT, coluna: INT, único: BOOL, raiz_da_btree: INT)
```

O **esquema** é um texto: colunas separadas por `;`, cada uma `nome,TIPO,not_null,primary_key`:

```
id,INT,1,1;nome,TEXT,0,0
```

O índice da chave primária é criado junto com a tabela e se chama `pk_<tabela>`.

Exemplo real (fim da página 1): as duas linhas do catálogo para `t`:

```
0f90  00 00 00 00 00 00 00 00 00 00 00 00 00 03 05 00  |................|
0fa0  00 00 69 6e 64 65 78 03 04 00 00 00 70 6b 5f 74  |..index.....pk_t|
0fb0  03 01 00 00 00 74 01 00 00 00 00 00 00 00 00 04  |.....t..........|
0fc0  01 01 03 00 00 00 00 00 00 00 03 05 00 00 00 74  |...............t|
0fd0  61 62 6c 65 03 01 00 00 00 74 01 02 00 00 00 00  |able.....t......|
0fe0  00 00 00 03 18 00 00 00 69 64 2c 49 4e 54 2c 31  |........id,INT,1|
0ff0  2c 31 3b 6e 6f 6d 65 2c 54 45 58 54 2c 30 2c 30  |,1;nome,TEXT,0,0|
```

* A linha `index` (45 bytes) começa em `0x0f9d`: `"index"`, `"pk_t"`, `"t"`, `Int(0)` (coluna 0),
  `Bool(true)` (único), `Int(3)` (a B-tree é a página 3).
* A linha `table` (54 bytes) começa em `0x0fca`: `"table"`, `"t"`, `Int(2)` (o heap da tabela é a
  página 2), esquema `id,INT,1,1;nome,TEXT,0,0` (o texto termina em `0x0fff`).
* O cabeçalho da página 1 (`00 00 00 00 02 00 9d 0f ...`) confere: `nslots = 2`, `free_end = 0x0f9d`; o
  slot 0 é `ca 0f 36 00` (offset `0x0fca`, len 54) e o slot 1, `9d 0f 2d 00` (offset `0x0f9d`, len 45).

---

## Página de B-tree

Cada nó ocupa uma página. Cabeçalho de **8 bytes**:

| Bytes | Campo | Significado |
|---|---|---|
| 0 | `kind` | `01` = folha, `02` = nó interno |
| 1 | `level` | `0` nas folhas; nos internos, nível dos filhos + 1 |
| 2..4 | `count` | número de **chaves** |
| 4..8 | `first` | folha: página da **próxima folha** (`0` = última); interno: **primeiro filho** |

Entradas, em ordem crescente de chave, logo após o cabeçalho:

| Tipo | Cada entrada |
|---|---|
| **Folha** | `len: u16` · `chave: len bytes` · `page: u32` · `slot: u16` (o `RowId`) |
| **Interno** | `len: u16` · `chave: len bytes` · `filho: u32` (o filho à **direita** da chave) |

* Num nó interno com `n` chaves há `n + 1` filhos: `first` e os `n` filhos das entradas.
  O filho `i` contém chaves `< chave[i]`; o filho `i+1`, chaves `>= chave[i]`.
* Tamanho máximo de uma chave: **512 bytes** (`MAX_KEY`). Um nó que passaria de 4096 bytes é dividido.
* A **raiz** nunca muda de página.

Exemplo real: a folha `pk_t` (página 3) com as chaves `1` e `2`:

```
0000  01 00 02 00 00 00 00 00 09 00 02 80 00 00 00 00  |................|
0010  00 00 01 02 00 00 00 00 00 09 00 02 80 00 00 00  |................|
0020  00 00 00 02 02 00 00 00 01 00 00 00 00 00 00 00  |................|
```

| Bytes | Leitura |
|---|---|
| `01` `00` `02 00` `00 00 00 00` | folha, nível 0, 2 chaves, `next = 0` |
| `09 00` | 1ª chave: 9 bytes |
| `02 80 00 00 00 00 00 00 01` | `INT 1` codificado (tag `02` + `0x8000…01` big-endian) |
| `02 00 00 00` `00 00` | `RowId{ page: 2, slot: 0 }` |
| `09 00` `02 80 .. 02` | 2ª chave: `INT 2` |
| `02 00 00 00` `01 00` | `RowId{ page: 2, slot: 1 }` |

---

## Codificação de chaves de índice

As chaves são **bytes comparáveis por `memcmp`**: comparar os bytes dá a mesma ordem que comparar os
valores ([ADR-0007](../adr/0007-chaves-memcomparable.md)). **Big-endian.**

| Valor | Bytes |
|---|---|
| `NULL` | `00` |
| `BOOL` | `01` + (`00` ou `01`) |
| `INT` | `02` + `(valor as u64) XOR 0x8000_0000_0000_0000` em big-endian |
| `FLOAT` | `03` + bits IEEE-754 em big-endian; se o bit de sinal é 1, inverte **todos** os bits, senão inverte só o bit de sinal |
| `TEXT` | `04` + bytes UTF-8 (cada `00` vira `00 FF`) + terminador `00 01` |

* **Índice único**: a chave é a codificação do valor (a B-tree recusa duplicatas). `NULL` num índice
  único usa a forma abaixo.
* **Índice não único**: `codificação ‖ page (u32 BE) ‖ slot (u16 BE)`, o que torna cada entrada única e
  as mantém agrupadas por valor.
* **Limites de intervalo**: `prefix_end(k) = k ‖ FF FF FF FF FF FF FF FF` é maior que qualquer entrada que
  comece com `k`; `[01]` é a primeira chave que não é `NULL`.

Exemplos:

| Valor | Chave |
|---|---|
| `INT 1` | `02 80 00 00 00 00 00 00 01` |
| `INT -1` | `02 7f ff ff ff ff ff ff ff` |
| `TEXT "a"` | `04 61 00 01` |
| `TEXT "a\0b"` | `04 61 00 ff 62 00 01` |

---

## Arquivo de log (WAL)

Uma sequência de **registros**, um por transação commitada, em ordem de commit:

```
registro = len: u32 · crc: u32 · payload (len bytes)
payload  = txn: u64 · nops: u32 · op × nops
```

* `crc` = **CRC-32 IEEE** (polinômio refletido `0xEDB88320`, inicial e final `0xFFFFFFFF`) **do payload**.
* Uma operação (`op`) começa por 1 byte de tipo:

| Tipo | Operação | Campos |
|---:|---|---|
| `01` | `HeapFormat` | `page: u32`: formata a página como página de heap vazia |
| `02` | `HeapLink` | `prev: u32`, `next: u32`: `prev.next = next` |
| `03` | `HeapInsert` | `page: u32`, `slot: u16`, `len: u32`, `len` bytes: coloca o registro **exatamente** nesse slot |
| `04` | `HeapDelete` | `page: u32`, `slot: u16`: libera o slot |

* **Só o heap é logado.** Os índices são reconstruídos na recuperação
  ([ADR-0004](../adr/0004-indices-reconstruidos-na-recuperacao.md)).
* Na abertura, o log é lido até o **primeiro** registro incompleto, com CRC errado ou que não
  decodifique; o resto é descartado e o arquivo é truncado ali.
* Depois de um checkpoint o arquivo tem **0 bytes**.

### Exemplo real: `CREATE TABLE t (...)` seguido de `INSERT` (222 bytes)

Registro do `CREATE TABLE` (146 bytes):

```
0000  8a 00 00 00 b8 79 80 8b 01 00 00 00 00 00 00 00  |.....y..........|
0010  03 00 00 00 01 02 00 00 00 03 01 00 00 00 00 00  |................|
0020  36 00 00 00 03 05 00 00 00 74 61 62 6c 65 03 01  |6........table..|
...
```

| Bytes | Leitura |
|---|---|
| `8a 00 00 00` | `len = 138` (o payload) |
| `b8 79 80 8b` | `crc = 0x8b8079b8` |
| `01 00 00 00 00 00 00 00` | `txn = 1` |
| `03 00 00 00` | `nops = 3` |
| `01` `02 00 00 00` | op 1: `HeapFormat{ page: 2 }` (o heap da tabela) |
| `03` `01 00 00 00` `00 00` `36 00 00 00` ... | op 2: `HeapInsert{ page: 1, slot: 0, len: 54 }` (a linha `table` do catálogo) |
| `03` `01 00 00 00` `01 00` `2d 00 00 00` ... | op 3: `HeapInsert{ page: 1, slot: 1, len: 45 }` (a linha `index` do catálogo) |

Registro do `INSERT` (76 bytes: 8 de cabeçalho + 68 de payload):

```
0092  44 00 00 00 cd 7b 93 e0 02 00 00 00 00 00 00 00  |D....{..........|
00a2  02 00 00 00 03 02 00 00 00 00 00 11 00 00 00 01  |................|
00b2  01 00 00 00 00 00 00 00 03 03 00 00 00 61 6e 61  |.............ana|
00c2  03 02 00 00 00 01 00 11 00 00 00 01 02 00 00 00  |................|
00d2  00 00 00 00 03 03 00 00 00 62 69 61              |.........bia|
```

`txn = 2`, `nops = 2`: `HeapInsert{ page: 2, slot: 0, len: 17 }` com a linha `(1,'ana')` e `HeapInsert{ page: 2,
slot: 1, len: 17 }` com `(2,'bia')`. Note que a criação da B-tree da chave primária **não** aparece
no log.

---

## Compatibilidade e versão

* O *magic* `RUSTYSQ2` é a única marca de versão. Mudar qualquer formato acima exige um novo magic;
  arquivos antigos são **recusados**, não migrados.
* Nada nos formatos é portável entre arquiteturas com *endianness* diferente (tudo é little-endian,
  exceto as chaves de índice).
