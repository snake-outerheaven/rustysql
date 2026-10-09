# Módulo `db`

> O motor: `Database`, `Session`, transações, recuperação, checkpoint e a execução de DDL/DML.

| | |
|---|---|
| **Arquivo** | `src/db.rs` (o maior módulo) |
| **Camada** | motor (topo; é a API pública) |
| **Depende de** | quase todos: `ast`, `btree`, `bufferpool`, `catalog`, `error`, `eval`, `exec`, `heap`, `keyenc`, `lockmgr`, `parser`, `plan`, `sched`, `schema`, `txn`, `value`, `wal` |
| **Usado por** | `main` e programas que embutem a biblioteca |
| **Testes** | todos os de `tests/` |

## 1. Responsabilidade

* **API pública**: abrir o banco, abrir sessões, executar SQL.
* **Ciclo de vida da transação**: `begin`, `commit`, `rollback`; atomicidade por comando.
* **Durabilidade**: recuperação na abertura e checkpoint.
* **Execução de comandos**: DDL e DML (o `SELECT` delega a [`plan`](plan.md) e [`exec`](exec.md)).
* **Cola de concorrência**: admissão ([`sched`](sched.md)), locks ([`lockmgr`](lockmgr.md)), portão de checkpoint.

## 2. API pública

### `Options`

| Campo | Padrão | Significado |
|---|---|---|
| `pool_pages` | 8192 (32 MB) | tamanho do buffer pool, em páginas |
| `sync_commits` | `true` | `fsync` do WAL a cada commit |
| `lock_timeout` | 5 s | espera máxima por um lock de tabela |
| `checkpoint_ratio` | 0.5 | fração de páginas sujas que dispara checkpoint automático |
| `sched` | `SchedConfig::default()` | admissão: `max_running` 64, `aging_step` 2 s, `queue_timeout` 30 s |

### `Database` (`Clone`, `Send + Sync`)

| Método | Descrição |
|---|---|
| `open(path)`, `open_with(path, opts)` | abre/cria (recupera se preciso) |
| `session() -> Session` | nova "conexão" |
| `run(sql) -> Vec<QueryResult>` | atalho: sessão descartável, autocommit |
| `checkpoint()` | grava páginas sujas e esvazia o WAL |
| `table_names()`, `describe(tabela)` | introspecção |
| `scheduler()`, `scheduler_stats()` | o escalonador e seus contadores |
| `lock_snapshot()` | quem segura/espera cada lock (diagnóstico) |
| `dirty_pages()`, `wal_bytes()` | estado do pool e do log |
| `simulate_crash()` | (testes) descarta sem checkpoint final |

### `Session`

| Método | Descrição |
|---|---|
| `execute(sql) -> Vec<QueryResult>` | executa um ou mais comandos; para no primeiro erro |
| `execute_stmt(&Statement)` | executa um comando já interpretado |
| `in_transaction()` | há transação explícita aberta? |
| `set_priority(p)`, `priority()` | prioridade de admissão da sessão |

### `QueryResult`: `Rows{columns, rows}` · `Affected(n)` · `Done(mensagem)`

## 3. Estruturas de dados

```rust
pub(crate) struct Inner {                 // estado compartilhado (atrás de um Arc)
    pool: Arc<BufferPool>,  wal: Wal,  locks: LockManager,  sched: Arc<Scheduler>,
    catalog: Arc<Catalog>,  catalog_heap: HeapFile,         // heap da página 1
    gate: Arc<RwLock<()>>,                                  // portão de checkpoint
    ddl:  Arc<Mutex<()>>,                                   // um DDL por vez
    next_txn: AtomicU64,  opts: Options,  crashed: AtomicBool,
}
pub struct Database { inner: Arc<Inner> }
pub struct Session  { db: Arc<Inner>, txn: Option<Txn>, priority: Priority }
```

## 4. Algoritmos e fluxos

### Abertura (`open_with`)
Ver o diagrama em [`runtime-scenarios.md`](../architecture/runtime-scenarios.md#4-abertura-do-banco-e-recuperação).
Resumo: abre pool e WAL; **banco novo** → cria o heap do catálogo (página 1); **existente** → reaplica cada
`WalOp` (`heap::apply_op`); carrega o catálogo; se houve registros no WAL, **reconstrói todos os índices**;
se o banco era novo ou foi recuperado, faz um **checkpoint**.

### Um comando (`Session::run_in_txn`)

```
auto = não há transação aberta
se auto:   txn = begin()                       ← pode fazer checkpoint; pega o portão
permit = sched.admit(InTransaction | New(prioridade))
exec(txn, stmt)        ── Ok  → auto: commit(txn)         | explícita: solta a vaga
                       ── Err → auto ou Deadlock/LockTimeout: rollback(txn)
                                senão: txn.rollback_to(marca) e segue
```

### `begin` / `commit` / `rollback` / `checkpoint`

| Função | O que faz |
|---|---|
| `begin(explicit, priority)` | se `páginas sujas > checkpoint_ratio × capacidade`, faz checkpoint; pega o portão (leitor) e cria o `Txn` |
| `commit(txn)` | grava o `WalOp` pendente num registro e `fsync`; executa `on_commit`; `release_all`; (o `Drop` do `Txn` solta portão, guard de DDL e vaga). Se a gravação falha: rollback |
| `rollback(txn)` | `rollback_to(Mark::start())`; grava as operações **estruturais** que sobraram; `release_all` |
| `checkpoint()` | portão **exclusivo** → `pool.flush_all()` → `wal.reset()` |
| `Inner::lock(txn, tabela, modo)` | `acquire_with`; ao bloquear, cede a vaga e depois a retoma |

### DML

* **`INSERT`**: lock `Exclusive`; resolve a lista de colunas; **avalia** cada expressão (constante) e
  **coage** ao tipo da coluna (`NOT NULL`, tipo; `INT → FLOAT` é permitido); só então insere linha a linha
  (`insert_row`: checa unicidade nos índices únicos, insere no heap, insere em **cada** índice, registrando
  undo e WAL).
* **`UPDATE`**: lock `Exclusive`; `collect_rids` materializa as linhas casadas; calcula as novas; **fase 1**:
  `delete_row` em todas as antigas; **fase 2**: `insert_row` das novas. Assim `SET id = id + 1` não colide
  com a própria tabela. Linhas cujo valor não mudou são puladas (mas contadas em `Affected`).
* **`DELETE`**: lock `Exclusive`; `collect_rids`; `delete_row` (índices primeiro, depois o heap).
* **`SELECT`**: lock `Shared` em cada tabela (em ordem de id); `plan_select`; `execute`; `collect`.

### DDL
Todos exigem transação **não explícita**, pegam o guard de DDL e publicam no catálogo em memória **no
commit** ([ADR-0014](../adr/0014-ddl-em-autocommit-e-publicacao-no-commit.md)):

| Comando | Efeito em disco (via WAL) | Publicação (`on_commit`) |
|---|---|---|
| `CREATE TABLE` | heap novo, linha no catálogo, índice `pk_<t>` se há chave primária | `catalog.publish(tabela)` |
| `DROP TABLE` | apaga as linhas do catálogo (tabela e índices) | `dropped = true`; `catalog.remove` |
| `CREATE INDEX` | árvore nova **carregada** a partir do heap; linha no catálogo | `tabela.indexes.push(índice)` |
| `DROP INDEX` | apaga a linha do catálogo (o índice da PK não pode ser removido) | `indexes.retain(...)` |

## 5. Invariantes

1. Toda transação segura o portão de checkpoint (lado leitor) do `begin` até ser descartada.
2. Um comando só altera uma tabela depois de ter o seu lock `Exclusive`.
3. O catálogo em memória só muda **depois** do commit durável do DDL.
4. Depois de qualquer comando que **falha**, o estado é o de antes do comando (atomicidade por comando).
5. `commit` só retorna `Ok` depois do registro do WAL estar no disco (se `sync_commits`).
6. `Drop` do último handle faz checkpoint (a menos que `simulate_crash` tenha sido chamado).

## 6. Concorrência

Ordem de aquisição de recursos por comando (nunca ao contrário, o que evita ciclos entre as camadas):

```
portão (leitor)  →  vaga de execução (Permit)  →  locks de tabela (em ordem de id)  →  latches de página
```

* O **checkpoint** pega o portão exclusivo: espera todas as transações e bloqueia as novas.
* `begin` roda **antes** da admissão ([ADR-0012](../adr/0012-controle-de-admissao-com-prioridade.md), regra 6).
* Mutexes internos (`catalog`, `ddl`, `wal`) são curtos.

## 7. Erros

Propaga os de todas as camadas. A política: `Deadlock`/`LockTimeout` → rollback da transação inteira;
outros → rollback do comando. `QueueTimeout` na admissão → o comando falha (e, em autocommit, a transação
vazia é descartada).

## 8. Testes

Todos: é o ponto de entrada. Mais diretamente: `basic::*`, `txn::*`, `recovery::*`, `sched::set_priority_*`,
os *doctests* de `Database`, `Session` e `Options`.

## 9. Limitações e evolução

* `db.rs` concentra muita coisa (≈1000 linhas): candidato a dividir em `session.rs`, `recovery.rs`, `ddl.rs`, `dml.rs`.
* O portão de checkpoint espera transações explícitas ociosas (D6).
* Sem *group commit* (D9), sem estatísticas de consulta, sem *prepared statements* (o texto é interpretado a cada execução).
* Sessões não são `Sync`: uma por thread.

## 10. Veja também

[`runtime-scenarios`](../architecture/runtime-scenarios.md), [ADR-0002](../adr/0002-no-steal-no-force-wal-redo.md),
[ADR-0013](../adr/0013-rollback-em-memoria-e-atomicidade-por-comando.md), [ADR-0014](../adr/0014-ddl-em-autocommit-e-publicacao-no-commit.md),
[`../guides/embedding-api.md`](../guides/embedding-api.md), [`../reference/configuration.md`](../reference/configuration.md)
