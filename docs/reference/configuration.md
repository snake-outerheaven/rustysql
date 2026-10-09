# Configuração

O rustysql é uma biblioteca embutida: **não há arquivo de configuração nem variáveis de ambiente**.
Tudo é configurado em código, na abertura do banco, por `Options`; e há algumas constantes de
compilação.

```rust
use rustysql::{Database, Options, SchedConfig};
use std::time::Duration;

let db = Database::open_with("meu.db", Options {
    pool_pages: 16_384,                          // 64 MB de cache
    sync_commits: true,
    lock_timeout: Duration::from_secs(10),
    checkpoint_ratio: 0.5,
    sched: SchedConfig { max_running: 32, ..SchedConfig::default() },
})?;
# Ok::<(), rustysql::Error>(())
```

O binário `rustysql` usa `Options::default()` (não expõe flags de linha de comando para isso).

## `Options`

| Campo | Tipo | Padrão | Significado |
|---|---|---|---|
| `pool_pages` | `usize` | `8192` (32 MB) | páginas de 4 KB no buffer pool (mínimo efetivo: 16) |
| `sync_commits` | `bool` | `true` | `fsync` do WAL a cada commit |
| `lock_timeout` | `Duration` | `5 s` | espera máxima por um lock de tabela → `Error::LockTimeout` |
| `checkpoint_ratio` | `f64` | `0.5` | fração de páginas sujas que dispara um checkpoint no próximo `begin` |
| `sched` | `SchedConfig` | veja abaixo | admissão por prioridade |

### `SchedConfig`

| Campo | Tipo | Padrão | Significado |
|---|---|---|---|
| `max_running` | `usize` | `64` | comandos executando ao mesmo tempo (mínimo 1) |
| `aging_step` | `Duration` | `2 s` | tempo de espera que compensa **um nível** de prioridade |
| `queue_timeout` | `Duration` | `30 s` | espera máxima na fila → `Error::QueueTimeout` |

## Como cada opção afeta o comportamento

### `pool_pages`: o limite de tamanho das transações

Por causa da política **no-steal**, páginas sujas só saem do pool num checkpoint. Logo:

* **uma transação não pode tocar mais páginas do que cabem no pool** (senão:
  `buffer pool cheio de páginas sujas/fixadas`);
* o pool **também** é o cache de leitura: mais páginas = menos leituras de disco.

Regra prática: `pool_pages ≥ 4 × (páginas que a maior transação toca)`. Uma linha de ~100 bytes ocupa
~1/40 de página; um `INSERT` de 100 mil linhas dessas toca ~2500 páginas de heap **mais** os índices.

### `sync_commits`

| Valor | Efeito | Use quando |
|---|---|---|
| `true` | cada commit só retorna depois do `fsync` do WAL: **durável** | produção, dados que importam |
| `false` | o SO pode manter o log em cache: uma queda do **sistema** (não do processo) pode perder commits recentes | testes, cargas descartáveis, importação em lote |

Mesmo com `false`, uma queda só do **processo** (o SO continua de pé) não perde nada: os dados já estão
no cache do SO.

### `checkpoint_ratio`

Quando `páginas_sujas > ratio × pool_pages`, o próximo `begin` faz um checkpoint (que **bloqueia** as
transações por um instante). Valores menores = checkpoints mais frequentes e menores (WAL curto,
recuperação rápida); maiores = menos pausas, WAL e recuperação maiores. Com `pool_pages` pequeno, o
padrão `0.5` já mantém metade do pool disponível para o trabalho da transação seguinte.

### `lock_timeout`

Rede de segurança: deadlocks são detectados na hora (não esperam o timeout). O timeout pega o caso de
uma transação **esquecida aberta** segurando um lock. Aumente se há transações longas legítimas.

### `sched.max_running`

Limita a concorrência de **comandos** (não de sessões). Com 1, a execução vira serial; com muitas sessões
e comandos curtos, o padrão (64) raramente enfileira. Diminua para proteger o sistema de sobrecarga;
aumente se há muito tempo ocioso esperando E/S.

### `sched.aging_step`

Quanto uma espera "vale" em prioridade. Com `2 s`: um comando `LOW` que esperou 4 s passa à frente de um
`HIGH` recém-chegado. **Menor** = prioridades mais "fracas" (a fila tende a FIFO); **maior** = a
prioridade domina por mais tempo.

### `sched.queue_timeout`

Quanto um comando aguarda uma vaga antes de desistir com `QueueTimeout`.

## Constantes de compilação

Mudam só alterando o código (e o formato do arquivo!):

| Constante | Valor | Onde | Efeito |
|---|---:|---|---|
| `PAGE_SIZE` | 4096 | `disk` | tamanho de página; **muda o formato do arquivo** |
| `MAX_RECORD` | 4084 | `heap` | maior linha serializada |
| `MAX_KEY` | 512 | `btree` | maior chave de índice **codificada** |
| `CATALOG_ROOT` | 1 | `db` | página raiz do catálogo |
| magic | `RUSTYSQ2` | `disk` | identifica o formato |

## Arquivos

| Arquivo | Nome |
|---|---|
| dados | o caminho dado a `Database::open` |
| log | o mesmo caminho + `.wal` (`meu.db` → `meu.db.wal`) |

Nenhum outro arquivo é criado. Ambos devem ficar **juntos**: copiar só o `.db` de um banco que não foi
fechado limpamente perde os commits que ainda estão só no `.wal`.

## Cargo

| Item | Valor |
|---|---|
| Edição | 2024 (Rust ≥ 1.85) |
| Dependências | `parking_lot` 0.12 (`arc_lock`, `send_guard`) |
| *Features* | nenhuma |
| Perfil de testes | `cargo test` (debug); para estresse use `cargo test --release` |
