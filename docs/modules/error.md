# Módulo `error`

> Um único tipo de erro para toda a biblioteca.

| | |
|---|---|
| **Arquivo** | `src/error.rs` |
| **Camada** | transversal |
| **Depende de** | nada (só `std`) |
| **Usado por** | quase todos os módulos |
| **Testes** | exercitado indiretamente por todos; mensagens verificadas em `recovery::a_transaction_larger_than_the_pool_...` |

## 1. Responsabilidade

Definir `Error` e `Result<T>`. **Não** decide *quando* um erro é fatal: essa política mora em
[`db`](db.md) (`Session::run_in_txn`).

## 2. API pública

| Item | Descrição |
|---|---|
| `enum Error` | as sete variantes abaixo |
| `type Result<T>` | `std::result::Result<T, Error>` |
| `Error::parse(msg)`, `exec(msg)`, `storage(msg)` | construtores de conveniência (`impl Into<String>`) |
| `impl Display`, `impl std::error::Error` | mensagens em português |
| `impl From<std::io::Error>` | permite `?` em operações de arquivo |

## 3. Estruturas de dados

| Variante | Categoria | Quando | Transitória? |
|---|---|---|---|
| `Io(std::io::Error)` | sistema | falha de leitura/escrita/`fsync` | não |
| `Parse(String)` | usuário | erro léxico ou sintático | não |
| `Exec(String)` | usuário | tabela inexistente, tipo errado, violação de restrição, divisão por zero... | não |
| `Storage(String)` | interna | arquivo corrompido, pool cheio, esquema ilegível | não |
| `Deadlock` | concorrência | esta transação foi escolhida como vítima de um ciclo de locks | **sim** |
| `LockTimeout` | concorrência | esperou um lock por mais de `Options::lock_timeout` | **sim** |
| `QueueTimeout` | concorrência | esperou uma vaga de execução por mais de `SchedConfig::queue_timeout` | **sim** |

Prefixos das mensagens (`Display`): `io error:`, `syntax error:`, `error:`, `storage error:`; as três
transitórias têm mensagem própria sem prefixo.

## 4. Algoritmos e fluxos

Sem lógica. Os fluxos de propagação são:

* `?` em quase toda a base de código;
* em `Session::run_in_txn`: `Deadlock` e `LockTimeout` → rollback **total**; demais erros → rollback do
  **comando** (ver [ADR-0013](../adr/0013-rollback-em-memoria-e-atomicidade-por-comando.md)).

## 5. Invariantes

* Erros de **usuário** (`Parse`, `Exec`) nunca corrompem o banco: o comando é desfeito.
* `Storage` sinaliza algo que "não deveria acontecer"; trate como defeito do banco ou do arquivo.

## 6. Concorrência

`Error` é `Send` (contém `io::Error` e `String`); pode atravessar threads.

## 7. Erros

Não se aplica (é o próprio tipo de erro). Catálogo para o usuário: [`../reference/errors.md`](../reference/errors.md).

## 8. Testes

Todo teste que verifica `is_err()` ou `matches!(.., Err(Error::X))` o exercita; os tipos transitórios:
`txn::deadlock_is_detected_and_one_victim_is_chosen`, `sched::queue_timeout_gives_up_and_does_not_block_those_behind`.

## 9. Limitações e evolução

* Mensagens são texto livre (`String`): não há códigos de erro estáveis (como `SQLSTATE`) para o
  chamador programar contra além das variantes.
* `Exec` mistura erros de usuário de naturezas diferentes (restrição, tipo, nome); separar em
  subvariantes facilitaria o tratamento por programa.

## 10. Veja também

[`../reference/errors.md`](../reference/errors.md), [ADR-0013](../adr/0013-rollback-em-memoria-e-atomicidade-por-comando.md)
