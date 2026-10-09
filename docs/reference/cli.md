# Referência do `rustysql` (linha de comando)

O binário `rustysql` é um REPL e um executor de scripts sobre a biblioteca.

```
rustysql [ARQUIVO.db [SCRIPT.sql]]
```

| Invocação | Modo |
|---|---|
| `rustysql` | REPL sobre `rustysql.db` (no diretório atual) |
| `rustysql meu.db` | REPL sobre `meu.db` |
| `rustysql meu.db roteiro.sql` | **modo script**: executa o arquivo e sai |

Com Cargo: `cargo run --release -- meu.db` (o `--` separa os argumentos do Cargo dos do programa).

O banco é criado se não existir; o log fica em `<arquivo>.wal`. Ao sair normalmente, o banco faz um
**checkpoint** e o `.wal` fica vazio.

## REPL

```
rustysql — meu.db  (.help para ajuda)
rustysql> CREATE TABLE t (id INT PRIMARY KEY, nome TEXT);
tabela 't' criada
rustysql> INSERT INTO t VALUES (1, 'ana'), (2, 'bia');
2 linha(s) afetada(s)
rustysql> SELECT * FROM t WHERE id = 2;
| id | nome |
|----|------|
| 2  | bia  |
(1 linha(s))
```

### Prompts

| Prompt | Estado |
|---|---|
| `rustysql> ` | pronto, sem transação aberta |
| `rustysql*> ` | **há uma transação explícita aberta** (depois de `BEGIN`) |
| `       ...> ` | continuação: o comando ainda não terminou em `;` |

Um comando só é executado quando a linha termina em `;`. Vários comandos na mesma linha são aceitos.

### Meta-comandos

Começam com `.` e valem **sozinhos numa linha** (fora de um comando em andamento). **Não** terminam em `;`.

| Comando | Efeito |
|---|---|
| `.tables` | lista as tabelas (ordem alfabética) |
| `.schema <tabela>` | colunas (com `PRIMARY KEY`/`NOT NULL`) e índices (`UNIQUE` quando for o caso) |
| `.stats` | páginas sujas e tamanho do WAL; fila de admissão (rodando/esperando, admitidos por prioridade, em transação, retomados, maior espera) |
| `.locks` | quem segura e quem espera cada lock de tabela (`nenhum lock ativo` se vazio) |
| `.help` | resumo dos comandos |
| `.exit`, `.quit` | sai (checkpoint final) |

Exemplos reais:

```
rustysql> .stats
páginas sujas: 0   WAL: 0 bytes
fila: 0 rodando, 0 esperando | admitidos low/normal/high: 0/0/0 | em transação: 0 | retomados: 0 | maior espera: 0.00ns
rustysql> .schema pessoas
  id INT PRIMARY KEY
  nome TEXT
  idade INT
  índice pk_pessoas em (id) UNIQUE
  índice idx_idade em (idade)
```

### Saída

* **Consulta**: tabela ASCII; cada coluna tem a largura da maior célula (ou do título); `NULL` aparece
  como `NULL`; termina com `(N linha(s))`.
* **`INSERT`/`UPDATE`/`DELETE`**: `N linha(s) afetada(s)`.
* **DDL e controle**: uma mensagem (`tabela 't' criada`, `BEGIN`, `COMMIT`, `PRIORITY HIGH`...).
* **`EXPLAIN`**: o plano em árvore (veja o [guia](../guides/query-tuning.md)).
* **Erro**: a mensagem (`error: ...`, `syntax error: ...`, ...) e o REPL **continua**; se havia uma
  transação aberta, ela segue aberta (exceto em `Deadlock`/`LockTimeout`, que a desfazem).

## Modo script

```
$ cargo run --release -- demo.db demo.sql
rustysql> CREATE TABLE pessoas (...);
tabela 'pessoas' criada
[2.01ms]

rustysql> SELECT * FROM pessoas WHERE id = 1999;
| id   | nome       | idade | dept_id |
|------|------------|-------|---------|
| 1999 | pessoa1999 | 19    | 4       |
(1 linha(s))
[118.60µs]
```

* O arquivo é dividido em `;` e cada comando é **ecoado** (truncado em 110 caracteres, com `...`),
  executado e seguido do **tempo** entre colchetes.
* Os comandos rodam **numa só sessão**, então `BEGIN ... COMMIT` funciona entre eles.
* Um erro num comando é impresso e o script **continua** com o próximo.
* **Limitação**: a divisão por `;` é ingênua: um `;` dentro de um texto (`'a;b'`) quebra o comando.

## Códigos de saída

| Código | Quando |
|---:|---|
| `0` | execução normal (inclusive quando comandos individuais falharam) |
| `1` | não conseguiu abrir o banco (`não consegui abrir 'x': ...`) ou ler o script |

## Exemplos

**Explorar um banco existente**

```
$ rustysql loja.db
rustysql> .tables
clientes
pedidos
rustysql> .schema pedidos
rustysql> SELECT COUNT(*) FROM pedidos;
```

**Transação no REPL**

```
rustysql> BEGIN;
BEGIN
rustysql*> UPDATE conta SET saldo = saldo - 10 WHERE id = 1;
1 linha(s) afetada(s)
rustysql*> ROLLBACK;
ROLLBACK
rustysql>
```

**Importar dados** (muito mais rápido numa só transação; veja `sync_commits` em
[`configuration.md`](configuration.md))

```
$ rustysql dados.db importar.sql
```

## Veja também

[`sql.md`](sql.md) · [`../guides/getting-started.md`](../guides/getting-started.md) · [`../modules/main.md`](../modules/main.md)
