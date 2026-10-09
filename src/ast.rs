//! Árvore sintática dos comandos SQL suportados.

use crate::sched::Priority;
use crate::schema::Column;
use crate::value::Value;
use std::fmt;

/// Operadores unários.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    /// Negação lógica (`NOT x`). `NOT NULL` continua `NULL`.
    Not,
    /// Negação aritmética (`-x`).
    Neg,
}

/// Operadores binários: aritméticos, de comparação e lógicos.
///
/// A precedência (da menor para a maior) é definida pelo parser: `OR` < `AND` < `NOT` <
/// comparações < `+ -` < `* / %`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    /// Soma (`+`). Entre dois textos, concatena.
    Add,
    /// Subtração (`-`).
    Sub,
    /// Multiplicação (`*`).
    Mul,
    /// Divisão (`/`): inteira entre inteiros, de ponto flutuante nos demais casos.
    /// Dividir inteiros por zero é um erro.
    Div,
    /// Resto da divisão (`%`).
    Mod,
    /// Igualdade (`=`).
    Eq,
    /// Diferença (`<>` ou `!=`).
    Ne,
    /// Menor que (`<`).
    Lt,
    /// Menor ou igual (`<=`).
    Le,
    /// Maior que (`>`).
    Gt,
    /// Maior ou igual (`>=`).
    Ge,
    /// Conjunção (`AND`), com lógica de três valores: `FALSE AND NULL` é `FALSE`.
    And,
    /// Disjunção (`OR`), com lógica de três valores: `TRUE OR NULL` é `TRUE`.
    Or,
}

/// Funções de agregação suportadas.
///
/// Todas ignoram `NULL` (exceto `COUNT(*)`). Sobre zero linhas, `COUNT` devolve `0` e as
/// demais devolvem `NULL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggFunc {
    /// `COUNT(*)` conta linhas; `COUNT(x)` conta os valores não nulos de `x`.
    Count,
    /// `SUM(x)`: `INT` se todos os valores são inteiros, `FLOAT` se algum for decimal.
    Sum,
    /// `AVG(x)`: média, sempre `FLOAT`.
    Avg,
    /// `MIN(x)`: menor valor, na ordem total de [`Value::total_cmp`].
    Min,
    /// `MAX(x)`: maior valor, na ordem total de [`Value::total_cmp`].
    Max,
}

/// Expressão SQL como saiu do parser, ainda **não ligada** às colunas de nenhuma tabela.
///
/// Antes de executar, ela passa por [`bind`](crate::eval::bind), que resolve os nomes de
/// coluna para posições e produz uma [`BExpr`](crate::eval::BExpr). Em `Display`, a expressão
/// é impressa de volta como SQL (usado nos nomes de colunas e no `EXPLAIN`).
#[derive(Debug, Clone)]
pub enum Expr {
    /// Literal: número, texto, booleano ou `NULL`.
    Lit(Value),
    /// Referência a uma coluna, opcionalmente qualificada (`t.col`).
    Col {
        /// Tabela ou alias que qualifica a coluna, se houver.
        table: Option<String>,
        /// Nome da coluna, em minúsculas.
        name: String,
    },
    /// Operação unária.
    Unary {
        /// O operador.
        op: UnOp,
        /// O operando.
        expr: Box<Expr>,
    },
    /// Operação binária.
    Binary {
        /// O operador.
        op: BinOp,
        /// Operando da esquerda.
        left: Box<Expr>,
        /// Operando da direita.
        right: Box<Expr>,
    },
    /// `expr IS [NOT] NULL`: o único teste que enxerga `NULL` como valor.
    IsNull {
        /// Expressão testada.
        expr: Box<Expr>,
        /// `true` para `IS NOT NULL`.
        negated: bool,
    },
    /// `expr [NOT] LIKE pattern`, com `%` (qualquer sequência) e `_` (um caractere).
    Like {
        /// Texto a casar.
        expr: Box<Expr>,
        /// Padrão.
        pattern: Box<Expr>,
        /// `true` para `NOT LIKE`.
        negated: bool,
    },
    /// `arg = None` é `COUNT(*)`
    Agg {
        /// A função de agregação.
        func: AggFunc,
        /// Argumento; `None` só em `COUNT(*)`.
        arg: Option<Box<Expr>>,
    },
}

impl Expr {
    /// Atalho para uma referência de coluna sem qualificador.
    pub fn col(name: &str) -> Expr {
        Expr::Col { table: None, name: name.to_string() }
    }

    /// Monta uma operação binária, cuidando do `Box` dos operandos.
    pub fn binary(op: BinOp, left: Expr, right: Expr) -> Expr {
        Expr::Binary { op, left: Box::new(left), right: Box::new(right) }
    }

    /// Há alguma função de agregação em qualquer ponto da expressão?
    pub fn contains_agg(&self) -> bool {
        match self {
            Expr::Agg { .. } => true,
            Expr::Lit(_) | Expr::Col { .. } => false,
            Expr::Unary { expr, .. } | Expr::IsNull { expr, .. } => expr.contains_agg(),
            Expr::Binary { left, right, .. } => left.contains_agg() || right.contains_agg(),
            Expr::Like { expr, pattern, .. } => expr.contains_agg() || pattern.contains_agg(),
        }
    }

    /// Todas as referências a colunas `(qualificador, nome)`.
    pub fn columns(&self, out: &mut Vec<(Option<String>, String)>) {
        match self {
            Expr::Lit(_) => {}
            Expr::Col { table, name } => out.push((table.clone(), name.clone())),
            Expr::Unary { expr, .. } | Expr::IsNull { expr, .. } => expr.columns(out),
            Expr::Binary { left, right, .. } => {
                left.columns(out);
                right.columns(out);
            }
            Expr::Like { expr, pattern, .. } => {
                expr.columns(out);
                pattern.columns(out);
            }
            Expr::Agg { arg, .. } => {
                if let Some(a) = arg {
                    a.columns(out);
                }
            }
        }
    }

    /// Quebra uma conjunção `a AND b AND c` em `[a, b, c]`.
    pub fn conjuncts(self) -> Vec<Expr> {
        match self {
            Expr::Binary { op: BinOp::And, left, right } => {
                let mut v = left.conjuncts();
                v.extend(right.conjuncts());
                v
            }
            other => vec![other],
        }
    }

    /// Junta predicados com `AND`; `None` se a lista está vazia. É o inverso de
    /// [`Expr::conjuncts`].
    pub fn and_all(mut parts: Vec<Expr>) -> Option<Expr> {
        let first = if parts.is_empty() { return None } else { parts.remove(0) };
        Some(parts.into_iter().fold(first, |acc, e| Expr::binary(BinOp::And, acc, e)))
    }
}

impl fmt::Display for BinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "%",
            BinOp::Eq => "=",
            BinOp::Ne => "<>",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "AND",
            BinOp::Or => "OR",
        })
    }
}

impl fmt::Display for AggFunc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            AggFunc::Count => "count",
            AggFunc::Sum => "sum",
            AggFunc::Avg => "avg",
            AggFunc::Min => "min",
            AggFunc::Max => "max",
        })
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Lit(Value::Text(s)) => write!(f, "'{s}'"),
            Expr::Lit(v) => write!(f, "{v}"),
            Expr::Col { table: Some(t), name } => write!(f, "{t}.{name}"),
            Expr::Col { table: None, name } => write!(f, "{name}"),
            Expr::Unary { op: UnOp::Not, expr } => write!(f, "NOT {expr}"),
            Expr::Unary { op: UnOp::Neg, expr } => write!(f, "-{expr}"),
            Expr::Binary { op, left, right } => write!(f, "({left} {op} {right})"),
            Expr::IsNull { expr, negated } => {
                write!(f, "{expr} IS {}NULL", if *negated { "NOT " } else { "" })
            }
            Expr::Like { expr, pattern, negated } => {
                write!(f, "{expr} {}LIKE {pattern}", if *negated { "NOT " } else { "" })
            }
            Expr::Agg { func, arg: Some(a) } => write!(f, "{func}({a})"),
            Expr::Agg { func, arg: None } => write!(f, "{func}(*)"),
        }
    }
}

/// Um item da lista de projeção do `SELECT`.
#[derive(Debug, Clone)]
pub enum SelectItem {
    /// `*`: todas as colunas de todas as tabelas do `FROM`.
    Star,
    /// `t.*`: todas as colunas de uma tabela (ou alias).
    QualifiedStar(String),
    /// Uma expressão, com alias opcional (`expr AS nome`).
    Expr {
        /// A expressão projetada.
        expr: Expr,
        /// Nome da coluna de saída, se informado.
        alias: Option<String>,
    },
}

/// Tabela citada no `FROM` ou num `JOIN`, com alias opcional.
#[derive(Debug, Clone)]
pub struct TableRef {
    /// Nome real da tabela, em minúsculas.
    pub name: String,
    /// Alias (`FROM emp e`); `None` se não houver.
    pub alias: Option<String>,
}

impl TableRef {
    /// Nome pelo qual a tabela é referenciada nas expressões: o alias, se houver;
    /// senão, o próprio nome.
    pub fn alias(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.name)
    }
}

/// Tipo de junção.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    /// `[INNER] JOIN`: só as combinações que satisfazem o `ON`.
    Inner,
    /// `LEFT [OUTER] JOIN`: mantém as linhas da esquerda sem par, estendidas com `NULL`.
    Left,
}

/// Uma cláusula `JOIN` do `SELECT`.
#[derive(Debug, Clone)]
pub struct Join {
    /// Tipo de junção.
    pub kind: JoinKind,
    /// Tabela que está sendo ligada.
    pub table: TableRef,
    /// Condição do `ON`.
    pub on: Expr,
}

/// Um `SELECT` completo.
///
/// A ordem lógica de execução é: `FROM`/`JOIN` → `WHERE` → `GROUP BY` → `HAVING` →
/// `ORDER BY` → projeção → `LIMIT`/`OFFSET`.
#[derive(Debug, Clone)]
pub struct SelectStmt {
    /// Lista de projeção.
    pub items: Vec<SelectItem>,
    /// Primeira tabela do `FROM`.
    pub from: TableRef,
    /// Junções, na ordem em que aparecem (associam à esquerda).
    pub joins: Vec<Join>,
    /// Condição do `WHERE`.
    pub filter: Option<Expr>,
    /// Expressões do `GROUP BY`.
    pub group_by: Vec<Expr>,
    /// Condição do `HAVING`, avaliada depois da agregação.
    pub having: Option<Expr>,
    /// Chaves do `ORDER BY` como `(expressão, descendente?)`.
    pub order_by: Vec<(Expr, bool)>, // (expressão, descendente?)
    /// `LIMIT`; `None` = sem limite.
    pub limit: Option<usize>,
    /// `OFFSET` (0 se omitido).
    pub offset: usize,
}

/// Um comando SQL já interpretado.
#[derive(Debug, Clone)]
pub enum Statement {
    /// `CREATE TABLE`.
    CreateTable {
        /// Nome da tabela.
        name: String,
        /// Definição das colunas, na ordem.
        columns: Vec<Column>,
    },
    /// `DROP TABLE`.
    DropTable {
        /// Tabela a remover.
        name: String,
    },
    /// `CREATE [UNIQUE] INDEX nome ON tabela(coluna)`.
    CreateIndex {
        /// Nome do índice (único no banco).
        name: String,
        /// Tabela indexada.
        table: String,
        /// Coluna indexada (um índice cobre uma só coluna).
        column: String,
        /// `true` para `UNIQUE`.
        unique: bool,
    },
    /// `DROP INDEX`.
    DropIndex {
        /// Índice a remover.
        name: String,
    },
    /// `INSERT INTO ... VALUES ...`.
    Insert {
        /// Tabela de destino.
        table: String,
        /// Lista de colunas; `None` = todas, na ordem da tabela.
        columns: Option<Vec<String>>,
        /// Uma expressão constante por valor, uma linha por entrada.
        rows: Vec<Vec<Expr>>,
    },
    /// `SELECT`.
    Select(Box<SelectStmt>),
    /// `UPDATE ... SET ... [WHERE ...]`.
    Update {
        /// Tabela alterada.
        table: String,
        /// Atribuições `coluna = expressão`.
        sets: Vec<(String, Expr)>,
        /// Condição do `WHERE`; `None` atualiza todas as linhas.
        filter: Option<Expr>,
    },
    /// `DELETE FROM ... [WHERE ...]`.
    Delete {
        /// Tabela afetada.
        table: String,
        /// Condição do `WHERE`; `None` apaga todas as linhas.
        filter: Option<Expr>,
    },
    /// `BEGIN`: abre uma transação explícita.
    Begin,
    /// `COMMIT`: confirma a transação explícita.
    Commit,
    /// `ROLLBACK`: desfaz a transação explícita.
    Rollback,
    /// `CHECKPOINT`: grava as páginas sujas e esvazia o WAL.
    Checkpoint,
    /// `SET PRIORITY LOW|NORMAL|HIGH`: prioridade de admissão desta sessão
    SetPriority(Priority),
    /// `EXPLAIN <comando>`: mostra o plano em vez de executar.
    Explain(Box<Statement>),
}
