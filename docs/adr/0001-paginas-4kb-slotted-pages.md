# ADR-0001: Páginas de 4 KB com *slotted pages* para o heap

* **Status**: aceita
* **Data**: 2026-10-09
* **Módulos afetados**: `disk`, `bufferpool`, `heap`, `btree`

## Contexto e declaração do problema

O banco precisa guardar linhas de tamanho variável (texto) num arquivo e achá-las depois por um
endereço estável, de modo que os índices possam apontar para elas. Também precisa de uma unidade de
E/S: ler e gravar o arquivo byte a byte seria impraticável e impossível de cachear.

## Fatores de decisão

* uma unidade de E/S que case com o disco e com o cache (o sistema operacional trabalha em páginas de 4 KB);
* endereço de linha **estável**: o índice não pode ser reescrito toda vez que uma página é reorganizada;
* inserir, apagar e reaproveitar espaço sem reescrever o arquivo todo;
* simplicidade didática.

## Opções consideradas

1. **Registros contíguos de tamanho fixo** num arquivo.
2. **Arquivo de log estruturado** (*append-only*, estilo LSM), com compactação em segundo plano.
3. **Páginas de 4 KB com *slotted pages*** (vetor de slots no início, registros crescendo do fim).

## Resultado da decisão

Escolhida a **opção 3**. O arquivo é um vetor de páginas de 4096 bytes (`PAGE_SIZE`); a página 0 é o
cabeçalho. Cada página de heap tem um cabeçalho de 8 bytes (próxima página, número de slots,
`free_end`), um **vetor de slots** `(offset, len)` crescendo do início e os **registros** crescendo do
fim. O endereço de uma linha é o `RowId = (página, slot)`.

### Consequências

* **Bom**: o `RowId` é estável. Apagar só zera o slot; compactar uma página muda os *offsets* mas nunca
  os índices de slot.
* **Bom**: slots livres são reaproveitados; o espaço dos registros apagados é recuperado por
  compactação da página (só quando o espaço contíguo não basta).
* **Bom**: a mesma página serve para heap e B-tree; o buffer pool é agnóstico.
* **Ruim**: um registro não pode passar de `MAX_RECORD` (4084 bytes); não há *overflow pages*.
* **Ruim**: tabelas são listas encadeadas de páginas: percorrer é sequencial e o `INSERT` precisa
  achar espaço (mitigado por uma dica `hint` com a primeira página que pode ter espaço).
* **Ruim**: um `DROP TABLE` não devolve as páginas (dívida D4 do arc42).

## Prós e contras das opções

### Registros de tamanho fixo
* Prós: trivial. Contras: desperdiça espaço com texto; não existe texto de tamanho fixo útil.

### Log estruturado
* Prós: escritas sequenciais, bom para SSD. Contras: leitura exige mesclar níveis; a compactação é
  um subsistema inteiro; foge do objetivo didático de um banco "clássico" com B-tree.

### Slotted pages
* Prós: é o desenho dos bancos relacionais tradicionais (PostgreSQL, SQLite, InnoDB); ensina o
  conceito central. Contras: fragmentação interna e limite de tamanho do registro.

## Validação

* `heap::apply_op` e `page::put_at` são exercitados por `recovery::replaying_the_log_twice_gives_the_same_result`.
* Reaproveitamento de slots e compactação: `basic::persistence_and_many_pages` (apaga metade e reinsere).
* Linhas maiores que a página são rejeitadas com `Error::Exec` (ver `HeapFile::insert`).

## Links

* [`modules/heap.md`](../modules/heap.md), [`reference/storage-formats.md`](../reference/storage-formats.md)
