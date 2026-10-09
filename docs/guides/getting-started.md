# Guia: primeiros passos

Em 10 minutos: compilar, abrir o terminal do banco, criar tabelas, usar índices e transações, e ver o
que o banco fez por baixo.

## Pré-requisitos

* **Rust 1.85 ou mais novo** (edição 2024): <https://rustup.rs>
* Nada mais: o projeto tem uma única dependência (`parking_lot`), baixada pelo Cargo.

```bash
git clone https://github.com/snake-outerheaven/rustysql.git
cd rustysql
cargo build --release
```

## 1. Abrir o terminal do banco

```bash
cargo run --release -- loja.db
```

```
rustysql — loja.db  (.help para ajuda)
rustysql>
```

O arquivo `loja.db` (e o log `loja.db.wal`) é criado no diretório atual. Para sair: `.exit`.

## 2. Criar uma tabela e inserir dados

```sql
CREATE TABLE clientes (
    id     INT PRIMARY KEY,
    nome   TEXT NOT NULL,
    cidade TEXT
);

INSERT INTO clientes VALUES
    (1, 'Ana',   'Recife'),
    (2, 'Beto',  'Salvador'),
    (3, 'Carla', 'Recife'),
    (4, 'Davi',  NULL);
```

Termine cada comando com `;` (o REPL espera até ver o `;`, então dá para quebrar em várias linhas).

## 3. Consultar

```sql
SELECT * FROM clientes WHERE cidade = 'Recife' ORDER BY nome DESC;
```

```
| id | nome  | cidade |
|----|-------|--------|
| 3  | Carla | Recife |
| 1  | Ana   | Recife |
(2 linha(s))
```

Repare que Davi (cidade `NULL`) **não** aparece: `NULL = 'Recife'` não é verdadeiro. Para achá-lo:
`WHERE cidade IS NULL`.

```sql
SELECT cidade, COUNT(*) AS total FROM clientes GROUP BY cidade ORDER BY total DESC;
```

## 4. Alterar e apagar

```sql
UPDATE clientes SET cidade = 'Olinda' WHERE id = 3;
DELETE FROM clientes WHERE nome = 'Davi';
```

## 5. Relacionar duas tabelas

```sql
CREATE TABLE pedidos (id INT PRIMARY KEY, cliente_id INT, valor FLOAT);
INSERT INTO pedidos VALUES (10, 1, 99.9), (11, 1, 20), (12, 2, 150.5);

SELECT c.nome, SUM(p.valor) AS gasto
FROM clientes c JOIN pedidos p ON p.cliente_id = c.id
GROUP BY c.nome ORDER BY gasto DESC;
```

```
| nome | gasto |
|------|-------|
| Beto | 150.5 |
| Ana  | 119.9 |
```

## 6. Ver o que o banco faz: `EXPLAIN` e índices

```sql
EXPLAIN SELECT * FROM pedidos WHERE cliente_id = 1;
```

```
Project [pedidos.id, pedidos.cliente_id, pedidos.valor]
  Filter (cliente_id = 1)
    SeqScan pedidos
```

`SeqScan` = leu a tabela inteira. Crie um índice e compare:

```sql
CREATE INDEX idx_pedido_cliente ON pedidos(cliente_id);
EXPLAIN SELECT * FROM pedidos WHERE cliente_id = 1;
```

```
Project [pedidos.id, pedidos.cliente_id, pedidos.valor]
  Filter (cliente_id = 1)
    IndexScan pedidos using idx_pedido_cliente (cliente_id = 1)
```

Detalhes e mais exemplos em [`query-tuning.md`](query-tuning.md).

## 7. Transações

```sql
BEGIN;
UPDATE clientes SET nome = 'Ana Maria' WHERE id = 1;
INSERT INTO clientes VALUES (1, 'duplicado', NULL);   -- erro: chave primária duplicada
COMMIT;
```

O `INSERT` falha, mas a transação **continua aberta** (o prompt é `rustysql*>`): só o comando que
falhou é desfeito. O `COMMIT` grava o `UPDATE`. Se preferir desistir de tudo, use `ROLLBACK`.

## 8. Inspecionar

| Comando | Mostra |
|---|---|
| `.tables` | tabelas |
| `.schema clientes` | colunas e índices |
| `.stats` | páginas sujas, tamanho do WAL, fila de admissão |
| `.locks` | quem segura cada lock (útil com várias sessões) |

## 9. Rodar um script

```bash
cargo run --release -- demo.db demo.sql
```

O repositório traz `demo.sql` (2000 linhas, índices, junções, transações). A saída mostra cada comando
e quanto ele demorou.

## 10. Rodar os testes

```bash
cargo test                 # tudo (≈ 52 testes)
cargo test --release       # os de concorrência ficam bem mais rápidos
cargo doc --no-deps --open # a referência de API gerada do código
```

## E agora?

| Quero... | Vá para |
|---|---|
| saber **todo** o SQL aceito | [`../reference/sql.md`](../reference/sql.md) |
| usar o banco **de dentro de um programa Rust** | [`embedding-api.md`](embedding-api.md) |
| entender transações e concorrência | [`concurrency-and-transactions.md`](concurrency-and-transactions.md) |
| deixar consultas mais rápidas | [`query-tuning.md`](query-tuning.md) |
| entender **como** o banco funciona | [`../architecture/arc42.md`](../architecture/arc42.md) |
