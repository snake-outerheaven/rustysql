# Módulo `main` (binário `rustysql`)

> REPL interativo e modo script.

| | |
|---|---|
| **Arquivo** | `src/main.rs` |
| **Camada** | interface |
| **Depende de** | a biblioteca `rustysql` (`Database`, `Session`, `QueryResult`) |
| **Usado por** | `cargo run` / o executável |
| **Testes** | manual (`demo.sql`); a lógica de banco é toda testada na biblioteca |

## 1. Responsabilidade

Ler SQL do teclado (ou de um arquivo), enviar para uma `Session` e imprimir os resultados em tabelas
ASCII, com tempos no modo script. Detalhes de uso em [`../reference/cli.md`](../reference/cli.md).

## 2. API pública

É um binário: a "API" são os argumentos e os meta-comandos.

```
rustysql [arquivo.db] [script.sql]
```

| Argumentos | Modo |
|---|---|
| nenhum | REPL sobre `rustysql.db` |
| `<db>` | REPL sobre o arquivo |
| `<db> <script>` | **script**: executa cada comando do arquivo, ecoando-o e mostrando o tempo |

Meta-comandos do REPL: `.tables`, `.schema <tabela>`, `.stats`, `.locks`, `.help`, `.exit`/`.quit`.

## 3. Estruturas de dados

Sem estado além da `Session` e de um *buffer* de linhas do comando em edição. O *prompt* indica o estado:

| Prompt | Significado |
|---|---|
| `rustysql> ` | pronto, sem transação |
| `rustysql*> ` | há uma transação explícita aberta |
| `       ...> ` | continuação do comando (não terminou em `;`) |

## 4. Algoritmos e fluxos

* **REPL**: lê linha; se o buffer está vazio e começa com `.`, é meta-comando; senão acumula até a linha
  terminar com `;` e executa tudo com `Session::execute`.
* **Script**: divide o arquivo em `;` (`split_inclusive`; **ingênuo**: um `;` dentro de uma string
  quebraria o comando), e para cada pedaço imprime `rustysql> <comando>` (truncado em 110 caracteres),
  executa, imprime o resultado e `[tempo]`.
* **Tabelas**: largura de cada coluna = maior entre o título e as células (contando caracteres, não
  bytes); `NULL` aparece como `NULL`.
* Ao sair (`.exit` ou fim da entrada), o `Database` é descartado e faz o **checkpoint final**.

## 5. Invariantes

Nenhum específico; a transação aberta numa sessão é desfeita se o REPL termina sem `COMMIT`.

## 6. Concorrência

Uma sessão, uma thread.

## 7. Erros

Erros do banco são impressos com o seu `Display` (`error: ...`, `syntax error: ...`) e o REPL continua.
Falha ao abrir o banco → mensagem em `stderr` e código de saída 1.

## 8. Testes

Manual: `cargo run --release -- demo.db demo.sql`.

## 9. Limitações e evolução

* Sem histórico/edição de linha (`readline`), sem autocompletar.
* O modo script divide por `;` ingenuamente.
* Sem formato de saída alternativo (CSV, JSON).

## 10. Veja também

[`../reference/cli.md`](../reference/cli.md), [`../guides/getting-started.md`](../guides/getting-started.md)
