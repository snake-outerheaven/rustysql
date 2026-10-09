# ADR-0003: Commit atômico: um registro de WAL por transação, com CRC

* **Status**: aceita
* **Data**: 2026-10-09
* **Módulos afetados**: `wal`, `txn`, `db`

## Contexto e declaração do problema

Uma transação que altera várias linhas gera várias operações de redo. Se cada operação fosse
gravada no log isoladamente, uma queda no meio deixaria **parte** da transação no log, e a
recuperação precisaria saber se o conjunto estava completo (um registro de `COMMIT` no fim) e
desfazer o que sobrou. Além disso, um registro gravado pela metade (queda no meio de um `write`)
precisa ser reconhecido como inválido.

## Fatores de decisão

* a recuperação deve ser simples: ou a transação existe inteira, ou não existe;
* detectar corrupção e gravação parcial;
* sem fase de undo no log ([ADR-0002](0002-no-steal-no-force-wal-redo.md)).

## Opções consideradas

1. **Registros por operação** + registro `COMMIT` + fase de análise na recuperação.
2. **Um registro por transação**, escrito no commit, contendo todas as operações.
3. Um registro por transação **sem** checksum (confiar no tamanho).

## Resultado da decisão

Escolhida a **opção 2**, com checksum. Durante a transação, as operações ficam em
`Txn::wal` (memória). No commit, `Wal::append_commit` grava **um** registro:

```
[len u32][crc32 u32][payload]
payload = txn u64 · nops u32 · ops...
```

A abertura (`Wal::open`) lê registros em sequência e **para no primeiro** que esteja incompleto, com
CRC errado ou que não decodifique, e trunca o arquivo ali.

### Consequências

* **Bom**: a atomicidade do commit vira atomicidade do registro: se o registro está inteiro (CRC
  confere), a transação foi commitada; senão, nunca existiu.
* **Bom**: não há registro de `COMMIT` nem fase de análise; o log é só uma lista de lotes.
* **Bom**: transações abortadas e abertas nunca chegam ao arquivo (exceto as operações estruturais:
  [ADR-0005](0005-operacoes-estruturais-sobrevivem-ao-rollback.md)).
* **Ruim**: o tamanho do registro é proporcional ao tamanho da transação (fica na memória até o commit).
* **Ruim**: um commit grande é uma escrita grande de uma vez.
* **Ruim**: um byte ruim no meio do arquivo descarta **tudo dali em diante**, inclusive commits
  posteriores válidos (comportamento padrão de WALs sequenciais; só é correto porque o log é
  truncado a cada checkpoint, então a janela é curta).

## Prós e contras das opções

### 1. Registros por operação
* Prós: transações maiores que a memória. Contras: mais código de recuperação; precisa de undo.

### 2. Um registro por transação (escolhida)
* Prós/contras: acima.

### 3. Sem checksum
* Contras: um tamanho íntegro com conteúdo lixo (setor corrompido) seria aplicado. Rejeitada.

## Validação

| Cenário | Teste |
|---|---|
| WAL cortado no meio do último registro | `recovery::a_torn_wal_tail_drops_only_the_unfinished_commit` |
| um byte alterado no último registro | `recovery::a_corrupted_wal_record_is_detected_by_the_checksum` |

## Links

* [`modules/wal.md`](../modules/wal.md), [`reference/storage-formats.md`](../reference/storage-formats.md#arquivo-de-log-wal)
