# Módulo `wal`

> Log de redo com um registro atômico (e CRC) por transação commitada.

| | |
|---|---|
| **Arquivo** | `src/wal.rs` |
| **Camada** | armazenamento |
| **Depende de** | `disk` (tipo `PageId`), `error` |
| **Usado por** | `heap` (tipo `WalOp`), `txn`, `db` |
| **Testes** | `recovery::*` (11 testes) |

## 1. Responsabilidade

* Definir as **operações de redo** (`WalOp`) e o formato do registro de commit (`Batch`).
* Gravar o registro de uma transação de forma **atômica e durável** (`append_commit`).
* Na abertura, ler os registros válidos e **descartar um final incompleto ou corrompido**.
* Esvaziar o log depois de um checkpoint (`reset`).

**Não faz**: aplicar as operações (é de `heap::apply_op`), decidir quando fazer checkpoint (é do
[`db`](db.md)), nem logar operações de índice ([ADR-0004](../adr/0004-indices-reconstruidos-na-recuperacao.md)).

## 2. API pública

| Item | Descrição |
|---|---|
| `enum WalOp { HeapFormat, HeapLink, HeapInsert, HeapDelete }` | operações de redo sobre o heap |
| `WalOp::is_structural()` | `HeapFormat` e `HeapLink` (sobrevivem ao rollback) |
| `struct Batch { txn, ops }` | um registro decodificado |
| `Wal::open(path, sync) -> (Wal, Vec<Batch>)` | abre, devolve os lotes válidos e trunca o resto |
| `Wal::append_commit(txn, &ops)` | grava um registro; `fsync` se `sync` |
| `Wal::reset()` | trunca para 0 e sincroniza |
| `Wal::len()`, `Wal::is_empty()` | tamanho do arquivo |

## 3. Estruturas de dados

Formato exato em [`../reference/storage-formats.md`](../reference/storage-formats.md#arquivo-de-log-wal).
Em resumo:

```
registro = [len: u32][crc32(payload): u32][payload]
payload  = [txn: u64][nops: u32] op*
op       = 1 Format{page u32}
         | 2 Link{prev u32, next u32}
         | 3 Insert{page u32, slot u16, len u32, bytes}
         | 4 Delete{page u32, slot u16}
```

CRC-32 IEEE (polinômio refletido `0xEDB88320`), calculado bit a bit (simples; o log é curto).

## 4. Algoritmos e fluxos

**`open`**: lê o arquivo inteiro e, em laço: precisa de 8 bytes de cabeçalho; lê `len`; confere que o
payload cabe no restante; confere o CRC; decodifica (`decode_batch`, que também exige consumir **todo**
o payload). No primeiro registro que falhar em qualquer passo, **para**: tudo dali em diante é
descartado e o arquivo é truncado nesse ponto (`set_len`). Posiciona a escrita no fim dos registros
válidos.

**`append_commit`**: codifica, calcula o CRC, monta `[len][crc][payload]` e faz **um** `write_all` sob
o mutex do arquivo; depois `sync_data` se configurado. Como só uma thread escreve por vez, a **ordem dos
registros no arquivo é a ordem de commit**.

**`reset`**: `set_len(0)`, volta ao início, `sync_data`.

## 5. Invariantes

1. Todo registro no arquivo está inteiro e com CRC válido (o que sobra de uma queda é cortado na abertura).
2. Os registros estão na ordem de commit.
3. O log só contém **commits** (e as operações estruturais de transações abortadas, que também
   foram gravadas como um commit; [ADR-0005](../adr/0005-operacoes-estruturais-sobrevivem-ao-rollback.md)).

## 6. Concorrência

`Wal` é `Sync`: um `Mutex<File>`. A seção crítica é a escrita (e o `fsync`); um *group commit* (vários
commits num só `fsync`) seria a melhoria natural (D9).

## 7. Erros

`Error::Io` em escrita/`fsync`. Um registro inválido na abertura **não** é erro: é o fim do log.

## 8. Testes

| Teste | O que cobre |
|---|---|
| `recovery::a_torn_wal_tail_drops_only_the_unfinished_commit` | arquivo cortado no meio do último registro |
| `recovery::a_corrupted_wal_record_is_detected_by_the_checksum` | um byte invertido |
| `recovery::committed_work_survives_a_crash_including_indexes` | ciclo completo commit → queda → redo |
| `recovery::replaying_the_log_twice_gives_the_same_result` | idempotência do redo |
| `recovery::checkpoint_empties_the_wal_and_later_commits_replay_on_top_of_it` | `reset` e novos commits |

## 9. Limitações e evolução

* Um byte ruim no meio do arquivo descarta tudo depois dele (aceitável porque o log é truncado a cada
  checkpoint).
* O log é lido inteiro na memória na abertura.
* Sem *group commit*, sem compressão, sem rotação de segmentos.
* O CRC bit a bit é lento para registros muito grandes; uma tabela de 256 entradas aceleraria.

## 10. Veja também

[`heap`](heap.md) (`apply_op`), [`txn`](txn.md), [`db`](db.md) (`open_with`, `commit`, `checkpoint`),
[ADR-0002](../adr/0002-no-steal-no-force-wal-redo.md), [ADR-0003](../adr/0003-commit-atomico-um-registro.md)
