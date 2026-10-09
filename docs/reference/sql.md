# Referência da linguagem SQL

O rustysql implementa um **subconjunto** de SQL. Este documento é a referência completa do que é
aceito, com a gramática, a semântica e exemplos. Tudo o que não está aqui **não é suportado** (veja
[Fora do escopo](#fora-do-escopo)).

## Sumário

* [Elementos léxicos](#elementos-léxicos)
* [Tipos de dados](#tipos-de-dados)
* [Gramática](#gramática)
* [Comandos](#comandos): DDL, DML, `SELECT`, transações, administração
* [Expressões e `NULL`](#expressões-e-null)
* [Agregações e agrupamento](#agregações-e-agrupamento)
* [Junções](#junções)
* [Ordenação e paginação](#ordenação-e-paginação)
* [Restrições e índices](#restrições-e-índices)
* [Fora do escopo](#fora-do-escopo)

---

## Elementos léxicos

| Elemento | Regra |
|---|---|
| Espaços | ignorados |
| Comentário | `-- até o fim da linha` |
| Identificador | letra ou `_`, seguidos de letras, dígitos e `_`; **sempre normalizado para minúsculas** (`Nome` e `NOME` são `nome`) |
| Palavra-chave | sem diferenciar maiúsculas (`select` = `SELECT`) |
| Inteiro | `42`, `-7` (o sinal é o operador unário `-`) |
| Decimal | `3.14`: **precisa** de dígito antes e depois do ponto |
| Texto | `'entre aspas simples'`; uma aspa dentro do texto se escreve `''` (`'D''Ávila'`) |
| Booleano | `TRUE`, `FALSE` |
| Nulo | `NULL` |
| Terminador | `;` separa comandos; o último pode omiti-lo |

> Identificadores **não** podem ser escritos entre aspas: não há como criar uma tabela chamada
> `"Minha Tabela"`.

## Tipos de dados

| Tipo | Aliases | Armazenamento | Observações |
|---|---|---|---|
| `INT` | `INTEGER`, `BIGINT` | inteiro de 64 bits com sinal | overflow aritmético é **erro** |
| `FLOAT` | `REAL`, `DOUBLE` | ponto flutuante de 64 bits | |
| `TEXT` | `VARCHAR`, `STRING` | UTF-8 | `VARCHAR(n)` aceita o `n` mas o **ignora** |
| `BOOL` | `BOOLEAN` | 1 byte | |

Toda coluna aceita `NULL`, salvo `NOT NULL` ou `PRIMARY KEY`.

**Coerção na gravação** (`INSERT` e `UPDATE`): um `INT` é aceito numa coluna `FLOAT` e convertido; **qualquer
outra** incompatibilidade é erro (`coluna 'x' é INT, mas recebeu Text("a")`). Não há conversão
automática de texto para número.

---

## Gramática

Notação EBNF: `[ ]` opcional · `{ }` zero ou mais · `|` alternativa · `"X"` palavra-chave (sem caixa) ·
`ident` identificador · `int` inteiro não negativo. Derivada de `src/parser.rs`.

```ebnf
script       = { ";" } [ comando { ";" { ";" } comando } ] { ";" } ;

comando      = explain | create_table | create_index | drop_table | drop_index
             | insert | select | update | delete
             | "BEGIN" [ "TRANSACTION" | "WORK" ] | "COMMIT" | "ROLLBACK" | "CHECKPOINT"
             | "SET" "PRIORITY" ident ;                (* ident: LOW | NORMAL | HIGH *)

explain      = "EXPLAIN" comando ;                    (* só SELECT, UPDATE e DELETE *)

create_table = "CREATE" "TABLE" ident "(" coluna { "," coluna } ")" ;
coluna       = ident tipo [ "(" int ")" ] { "PRIMARY" "KEY" | "NOT" "NULL" | "NULL" } ;
tipo         = "INT" | "INTEGER" | "BIGINT" | "FLOAT" | "REAL" | "DOUBLE"
             | "TEXT" | "VARCHAR" | "STRING" | "BOOL" | "BOOLEAN" ;
create_index = "CREATE" [ "UNIQUE" ] "INDEX" ident "ON" ident "(" ident ")" ;
drop_table   = "DROP" "TABLE" ident ;
drop_index   = "DROP" "INDEX" ident ;

insert       = "INSERT" "INTO" ident [ "(" ident { "," ident } ")" ]
               "VALUES" linha { "," linha } ;
linha        = "(" expr { "," expr } ")" ;

select       = "SELECT" item { "," item }
               "FROM" tabela { juncao }
               [ "WHERE" expr ]
               [ "GROUP" "BY" expr { "," expr } ]
               [ "HAVING" expr ]
               [ "ORDER" "BY" ordem { "," ordem } ]
               [ "LIMIT" int ] [ "OFFSET" int ] ;
item         = "*" | ident "." "*" | expr [ "AS" ident ] ;
tabela       = ident [ [ "AS" ] ident ] ;
juncao       = [ "INNER" | "LEFT" [ "OUTER" ] ] "JOIN" tabela "ON" expr ;
ordem        = expr [ "ASC" | "DESC" ] ;

update       = "UPDATE" ident "SET" atrib { "," atrib } [ "WHERE" expr ] ;
atrib        = ident "=" expr ;
delete       = "DELETE" "FROM" ident [ "WHERE" expr ] ;

expr         = e_and { "OR" e_and } ;
e_and        = e_not { "AND" e_not } ;
e_not        = "NOT" e_not | comparacao ;
comparacao   = soma [ "IS" [ "NOT" ] "NULL"
                    | [ "NOT" ] "BETWEEN" soma "AND" soma
                    | [ "NOT" ] "IN" "(" soma { "," soma } ")"
                    | [ "NOT" ] "LIKE" soma
                    | ( "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=" ) soma ] ;
soma         = produto { ( "+" | "-" ) produto } ;
produto      = unario { ( "*" | "/" | "%" ) unario } ;
unario       = "-" unario | primario ;
primario     = int | decimal | texto | "TRUE" | "FALSE" | "NULL"
             | "(" expr ")" | [ ident "." ] ident | agregacao ;
agregacao    = ( "COUNT" | "SUM" | "AVG" | "MIN" | "MAX" ) "(" ( expr | "*" ) ")" ;
                                                      (* "*" só em COUNT *)
```

**Palavras reservadas como alias** (um identificador logo depois do nome de uma tabela só é alias se
**não** for uma destas): `select from where group order by having limit offset join inner left outer on
set values and or not as union`. Fora disso, palavras-chave **não** são reservadas: uma coluna pode se
chamar `count` ou `key`.

---

## Comandos

### `CREATE TABLE`

```sql
CREATE TABLE emp (
    id      INT PRIMARY KEY,
    nome    TEXT NOT NULL,
    salario FLOAT,
    ativo   BOOL
);
```

* No máximo **uma** `PRIMARY KEY`, de **uma** coluna (implica `NOT NULL`); ela cria o índice único
  `pk_<tabela>` automaticamente.
* Nome de tabela repetido, coluna repetida ou tabela sem colunas são erros.
* DDL só roda em **autocommit** (não dentro de `BEGIN`).

### `DROP TABLE`, `CREATE INDEX`, `DROP INDEX`

```sql
CREATE INDEX idx_salario ON emp(salario);        -- índice comum (admite repetidos)
CREATE UNIQUE INDEX idx_nome ON emp(nome);       -- falha se já houver valores repetidos
DROP INDEX idx_salario;
DROP TABLE emp;
```

* Um índice cobre **uma** coluna; o nome é único em **todo** o banco.
* O índice da chave primária (`pk_<tabela>`) **não** pode ser removido.
* `CREATE INDEX` constrói o índice a partir dos dados existentes, sob lock exclusivo da tabela.

### `INSERT`

```sql
INSERT INTO emp VALUES (1, 'ana', 100.0, TRUE), (2, 'bia', 250, NULL);
INSERT INTO emp (id, nome) VALUES (3, 'caio');    -- as outras colunas ficam NULL
```

* Os valores são **expressões constantes** (`10 * 2`, `-5`, `'a'`); não podem citar colunas.
* Quantidade de valores diferente da de colunas → erro.
* O comando é **atômico**: se uma linha viola uma restrição, nenhuma é inserida.

### `UPDATE`

```sql
UPDATE emp SET salario = salario * 1.1 WHERE ativo;
UPDATE emp SET id = id + 1000;                    -- permitido: mudar a chave em lote
```

* O lado direito pode citar colunas **da linha antiga**.
* Mudar a chave primária em lote não gera falsa colisão: o motor apaga todas as versões antigas antes de
  inserir as novas. Uma colisão **de verdade** é erro e desfaz tudo.
* `Affected(n)` conta as linhas **casadas**, inclusive as que ficaram com o mesmo valor.

### `DELETE`

```sql
DELETE FROM emp WHERE salario IS NULL;
DELETE FROM emp;                                  -- apaga todas as linhas
```

### `SELECT`

Ver [Junções](#junções), [Agregações](#agregações-e-agrupamento) e
[Ordenação](#ordenação-e-paginação). Sempre exige `FROM`.

```sql
SELECT nome, salario * 12 AS anual FROM emp WHERE salario > 100 ORDER BY anual DESC LIMIT 5;
SELECT e.*, d.nome FROM emp e JOIN depts d ON e.dept_id = d.id;
```

O **nome** da coluna de saída é o alias (`AS`), senão o nome da coluna, senão o texto da expressão
(`salario * 12` aparece como `(salario * 12)`).

### Transações

```sql
BEGIN;                    -- ou BEGIN TRANSACTION / BEGIN WORK
UPDATE conta SET saldo = saldo - 10 WHERE id = 1;
UPDATE conta SET saldo = saldo + 10 WHERE id = 2;
COMMIT;                   -- ou ROLLBACK
```

* Fora de `BEGIN`, cada comando é uma transação (**autocommit**).
* Um comando que **falha** dentro de uma transação desfaz **só ele**; a transação continua aberta.
* `Deadlock` e `LockTimeout` abortam a transação **inteira** (repita-a do início).
* `BEGIN` dentro de transação, `COMMIT`/`ROLLBACK` sem transação e `CHECKPOINT` dentro de transação são erros.

### Administração

| Comando | Efeito |
|---|---|
| `CHECKPOINT` | grava as páginas sujas no arquivo e esvazia o WAL |
| `SET PRIORITY LOW\|NORMAL\|HIGH` | prioridade de admissão dos próximos comandos **desta sessão** (padrão `NORMAL`) |
| `EXPLAIN <SELECT\|UPDATE\|DELETE>` | mostra o plano em vez de executar ([guia](../guides/query-tuning.md)) |

---

## Expressões e `NULL`

### Operadores, do de **menor** para o de **maior** precedência

| Nível | Operadores | Associatividade |
|---:|---|---|
| 1 | `OR` | esquerda |
| 2 | `AND` | esquerda |
| 3 | `NOT` | prefixo |
| 4 | `=` `<>` `!=` `<` `<=` `>` `>=` `IS [NOT] NULL` `[NOT] BETWEEN` `[NOT] IN` `[NOT] LIKE` | não associativos |
| 5 | `+` `-` | esquerda |
| 6 | `*` `/` `%` | esquerda |
| 7 | `-` (unário) | prefixo |

### `NULL` e lógica de três valores

`NULL` significa "desconhecido". Qualquer comparação ou aritmética com `NULL` dá `NULL`; um `WHERE`/`ON`/
`HAVING` só deixa passar linhas cujo resultado é **exatamente `TRUE`**.

| Expressão | Resultado |
|---|---|
| `1 = NULL` | `NULL` (não `TRUE`, não `FALSE`) |
| `NULL = NULL` | `NULL` |
| `x IS NULL` | `TRUE`/`FALSE` (o único teste que "vê" o `NULL`) |
| `TRUE OR NULL` | `TRUE` |
| `FALSE AND NULL` | `FALSE` |
| `NOT NULL` | `NULL` |
| `NULL + 1` | `NULL` |

Tabelas-verdade completas: [`modules/eval.md`](../modules/eval.md#lógica-de-três-valores).

### Comparações e tipos

* `INT` e `FLOAT` comparam entre si numericamente.
* `TEXT` compara pelos **bytes UTF-8** (sem *collation*): `'Z' < 'a'`.
* `BOOL` compara com `BOOL` (`FALSE < TRUE`).
* Comparar tipos incompatíveis (`1 = 'a'`) é **erro**, não `FALSE`.

### Açúcar sintático

| Escrito | Equivale a |
|---|---|
| `x BETWEEN a AND b` | `x >= a AND x <= b` |
| `x IN (a, b)` | `x = a OR x = b` (**não** usa índice) |
| `x NOT BETWEEN/IN/LIKE ...` | `NOT (...)` |

### `LIKE`

`%` casa qualquer sequência (inclusive vazia), `_` casa **um** caractere. **Diferencia maiúsculas**; não há
escape nem `ILIKE`. `'ana' LIKE 'a%'` → `TRUE`; `'ana' LIKE '_n_'` → `TRUE`.

### Aritmética

| Operandos | Resultado |
|---|---|
| `INT op INT` | `INT` (`/` é divisão inteira); **overflow** e divisão/módulo por **zero** → erro |
| `INT` com `FLOAT` | `FLOAT` |
| `TEXT + TEXT` | concatenação |
| outras combinações | erro |

---

## Agregações e agrupamento

| Função | Vazio | Ignora `NULL` | Resultado |
|---|---|---|---|
| `COUNT(*)` | `0` | não (conta linhas) | `INT` |
| `COUNT(x)` | `0` | sim | `INT` |
| `SUM(x)` | `NULL` | sim | `INT` (erro em overflow) ou `FLOAT` se houver decimais |
| `AVG(x)` | `NULL` | sim | `FLOAT` |
| `MIN(x)`, `MAX(x)` | `NULL` | sim | tipo de `x` |

* Uma consulta com agregação **e sem** `GROUP BY` produz **uma linha** (mesmo sobre zero linhas).
* Com `GROUP BY`, `NULL`s caem no **mesmo** grupo.
* **Colunas não agrupadas** podem ser usadas no `SELECT` (não é um erro, como no SQLite/MySQL antigo): o
  valor é o da **primeira linha do grupo**. Não dependa disso.
* `HAVING` filtra grupos depois da agregação e pode usar agregações (`HAVING COUNT(*) > 1`).
* Não existe `DISTINCT` (nem `COUNT(DISTINCT x)`).
* Agregações **não** são permitidas em `WHERE` nem em `ON`.

```sql
SELECT dept_id, COUNT(*) AS n, AVG(salario)
FROM emp GROUP BY dept_id HAVING COUNT(*) > 1 ORDER BY n DESC;
```

---

## Junções

```sql
SELECT e.nome, d.nome FROM emp e JOIN depts d ON e.dept_id = d.id;          -- INNER
SELECT e.nome, d.nome FROM emp e LEFT JOIN depts d ON e.dept_id = d.id;     -- LEFT OUTER
```

* `JOIN` = `INNER JOIN`. Há **só** `INNER` e `LEFT`; não há `RIGHT`, `FULL`, `CROSS`, `NATURAL` nem `USING`;
  junção por vírgula (`FROM a, b`) não existe.
* Várias junções associam **à esquerda**, na ordem escrita: `a JOIN b JOIN c` = `(a JOIN b) JOIN c`.
  A ordem **não** é reordenada pelo planejador.
* Num `LEFT JOIN`, as linhas da esquerda sem par aparecem com `NULL` nas colunas da direita; um `WHERE`
  sobre a tabela da direita roda **depois** da junção (e portanto descarta essas linhas).
* Alias repetido (`FROM emp e JOIN depts e`) é erro; coluna ambígua (`nome` nas duas tabelas) é erro: use
  `e.nome`.
* O algoritmo (índice, hash ou laço aninhado) é escolhido pelo planejador: veja `EXPLAIN`.

---

## Ordenação e paginação

* `ORDER BY` aceita expressões, **alias** de saída e **posição** (`ORDER BY 2`); `ASC` é o padrão.
* `NULL` ordena **primeiro** em `ASC` (e por último em `DESC`).
* A ordenação é **estável**.
* `LIMIT n [OFFSET m]` (`OFFSET` também pode aparecer sozinho). Sem `ORDER BY`, a ordem das linhas **não é
  garantida**.

---

## Restrições e índices

| Restrição | Efeito | Erro |
|---|---|---|
| `PRIMARY KEY` | único, `NOT NULL`, indexado | `chave primária duplicada: 5` |
| `NOT NULL` | recusa `NULL` | `coluna 'nome' não aceita NULL` |
| `CREATE UNIQUE INDEX` | único; **vários `NULL`s são permitidos** | `valor duplicado no índice único 'idx': ...` |
| tipo da coluna | coerção `INT → FLOAT`; resto é erro | `coluna 'x' é INT, mas recebeu ...` |

Uma violação **desfaz o comando inteiro** (não só a linha ruim).

---

## Fora do escopo

Não implementados: `ALTER TABLE`, `DEFAULT`, `CHECK`, chaves estrangeiras, `AUTOINCREMENT`, índices
compostos (mais de uma coluna), mais de uma `PRIMARY KEY`, `SELECT` sem `FROM`, subconsultas, `DISTINCT`,
`UNION`/`INTERSECT`/`EXCEPT`, `CASE`, `CAST`, funções escalares (`UPPER`, `COALESCE`, `LENGTH`...),
funções de janela, `CTE` (`WITH`), visões, gatilhos, `RIGHT`/`FULL`/`CROSS JOIN`, `USING`, tipos de data e
hora, `DECIMAL` exato, `BLOB`, usuários e permissões, *prepared statements*.
