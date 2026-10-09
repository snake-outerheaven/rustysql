# Módulo `lexer`

> Quebra o texto SQL em tokens.

| | |
|---|---|
| **Arquivo** | `src/lexer.rs` |
| **Camada** | SQL |
| **Depende de** | `error` |
| **Usado por** | `parser` |
| **Testes** | indireto: todo SQL passa por ele; erros léxicos em `basic`/`sql` |

## 1. Responsabilidade

Transformar `&str` em `Vec<Token>`, ignorando espaços e comentários. **Não** distingue palavras-chave
de identificadores (isso é do [`parser`](parser.md), que compara sem diferenciar maiúsculas).

## 2. API pública

| Item | Descrição |
|---|---|
| `tokenize(&str) -> Result<Vec<Token>>` | o analisador léxico |
| `enum Token` | `Ident(String)`, `Int(i64)`, `Float(f64)`, `Str(String)`, `LParen`, `RParen`, `Comma`, `Dot`, `Semi`, `Star`, `Plus`, `Minus`, `Slash`, `Percent`, `Eq`, `Ne`, `Lt`, `Le`, `Gt`, `Ge` |

## 3. Estruturas de dados

Regras léxicas:

| Token | Regra |
|---|---|
| espaços | ignorados |
| comentário | `--` até o fim da linha |
| `Ident` | `[A-Za-z_]` (ou qualquer letra Unicode) seguido de letras, dígitos e `_` |
| `Int` | dígitos; fora do intervalo de `i64` → erro |
| `Float` | dígitos `.` dígitos (**exige** dígito depois do ponto: `1.` não é decimal; `t.col` é `Ident Dot Ident`) |
| `Str` | `'...'`; a aspa dentro do texto é escrita `''` |
| `Ne` | `<>` ou `!=` |
| `Le`, `Ge` | `<=`, `>=` |
| `Eq`, `Lt`, `Gt`, `Plus`, `Minus`, `Star`, `Slash`, `Percent`, `Comma`, `Dot`, `Semi`, `LParen`, `RParen` | um caractere |

## 4. Algoritmos e fluxos

Um laço único sobre um `Vec<char>` com um índice `i`; cada ramo avança `i`. Números: consome dígitos e,
se há `.` seguido de dígito, a parte decimal. Strings: laço até a aspa de fechamento, tratando `''`.

## 5. Invariantes

* Todo caractere de entrada é consumido ou gera erro: não há caracteres "esquecidos".
* A saída nunca contém espaços nem comentários.

## 6. Concorrência

Função pura.

## 7. Erros

`Error::Parse`: `string sem fechar`, `número inválido`, `inteiro fora do intervalo`,
`caractere inesperado 'x'`.

## 8. Testes

Sem teste unitário próprio; coberto por todos os testes que executam SQL, e pelo *doctest* de
`parser::parse` (`parse("SELEC * FROM t")` falha).

## 9. Limitações e evolução

* Sem literais hexadecimais, notação científica (`1e5`) ou strings com escape `\n`.
* Sem identificadores entre aspas (`"Meu Nome"`): todo identificador é normalizado para minúsculas.
* Sem posição (linha/coluna) nos erros.

## 10. Veja também

[`parser`](parser.md), [`../reference/sql.md`](../reference/sql.md#elementos-léxicos)
