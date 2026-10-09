use rustysql::{Database, QueryResult, Session};
use std::io::{self, BufRead, Write};
use std::time::Instant;

const HELP: &str = "\
Comandos:
  .tables             lista as tabelas
  .schema <tabela>    colunas e índices
  .stats              páginas sujas, WAL e fila de admissão
  .locks              quem segura e quem espera cada lock
  .help               esta ajuda
  .exit               sai (faz checkpoint)
SQL (termine com ';'):
  CREATE TABLE | CREATE [UNIQUE] INDEX | DROP TABLE | DROP INDEX
  INSERT | SELECT (JOIN, GROUP BY, HAVING, ORDER BY, LIMIT) | UPDATE | DELETE
  BEGIN | COMMIT | ROLLBACK | CHECKPOINT | EXPLAIN <comando>
  SET PRIORITY LOW|NORMAL|HIGH   (prioridade de admissão desta sessão)";

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "rustysql.db".to_string());
    let script = args.next();

    let db = match Database::open(&path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("não consegui abrir '{path}': {e}");
            std::process::exit(1);
        }
    };
    let mut session = db.session();

    // modo script: `rustysql <db> <arquivo.sql>` executa e ecoa cada comando
    if let Some(script) = script {
        let sql = std::fs::read_to_string(&script).unwrap_or_else(|e| {
            eprintln!("não consegui ler '{script}': {e}");
            std::process::exit(1);
        });
        for stmt in sql.split_inclusive(';').map(str::trim).filter(|s| !s.is_empty()) {
            let shown: String = stmt.chars().take(110).collect();
            println!("rustysql> {shown}{}", if shown.len() < stmt.len() { " ..." } else { "" });
            let start = Instant::now();
            run(&mut session, stmt);
            println!("[{:.2?}]\n", start.elapsed());
        }
        return;
    }

    println!("rustysql — {path}  (.help para ajuda)");
    let stdin = io::stdin();
    let mut buffer = String::new();
    loop {
        let prompt = match (buffer.is_empty(), session.in_transaction()) {
            (true, false) => "rustysql> ",
            (true, true) => "rustysql*> ",
            (false, _) => "       ...> ",
        };
        print!("{prompt}");
        io::stdout().flush().ok();

        let mut line = String::new();
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let trimmed = line.trim();

        if buffer.is_empty() && trimmed.starts_with('.') {
            let mut parts = trimmed.split_whitespace();
            match parts.next().unwrap() {
                ".exit" | ".quit" => break,
                ".help" => println!("{HELP}"),
                ".tables" => db.table_names().iter().for_each(|t| println!("{t}")),
                ".stats" => {
                    let s = db.scheduler_stats();
                    println!("páginas sujas: {}   WAL: {} bytes", db.dirty_pages(), db.wal_bytes());
                    println!(
                        "fila: {} rodando, {} esperando | admitidos low/normal/high: {}/{}/{} | em transação: {} | retomados: {} | maior espera: {:.2?}",
                        s.running, s.queued, s.admitted[0], s.admitted[1], s.admitted[2],
                        s.admitted_in_txn, s.resumed, s.max_wait
                    );
                }
                ".locks" => {
                    let snap = db.lock_snapshot();
                    print!("{}", if snap.is_empty() { "nenhum lock ativo\n".to_string() } else { snap });
                }
                ".schema" => match parts.next().and_then(|t| db.describe(&t.to_lowercase())) {
                    Some(d) => {
                        for c in &d.columns {
                            let mut flags = String::new();
                            if c.primary_key {
                                flags.push_str(" PRIMARY KEY");
                            } else if c.not_null {
                                flags.push_str(" NOT NULL");
                            }
                            println!("  {} {}{}", c.name, c.ty.name(), flags);
                        }
                        for (name, col, unique) in &d.indexes {
                            println!("  índice {name} em ({col}){}", if *unique { " UNIQUE" } else { "" });
                        }
                    }
                    None => println!("uso: .schema <tabela existente>"),
                },
                other => println!("comando desconhecido: {other}"),
            }
            continue;
        }

        buffer.push_str(&line);
        if !buffer.trim_end().ends_with(';') {
            continue;
        }
        run(&mut session, &buffer);
        buffer.clear();
    }
}

fn run(session: &mut Session, sql: &str) {
    match session.execute(sql) {
        Ok(results) => results.iter().for_each(print_result),
        Err(e) => println!("{e}"),
    }
}

fn print_result(r: &QueryResult) {
    match r {
        QueryResult::Done(msg) => println!("{msg}"),
        QueryResult::Affected(n) => println!("{n} linha(s) afetada(s)"),
        QueryResult::Rows { columns, rows } => {
            let cells: Vec<Vec<String>> =
                rows.iter().map(|r| r.iter().map(|v| v.to_string()).collect()).collect();
            let widths: Vec<usize> = columns
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    cells.iter().map(|r| r[i].chars().count()).chain([c.chars().count()]).max().unwrap()
                })
                .collect();
            let line = |vals: &[String]| {
                let parts: Vec<String> = vals
                    .iter()
                    .zip(&widths)
                    .map(|(v, w)| format!("{v:<w$}", w = *w))
                    .collect();
                println!("| {} |", parts.join(" | "));
            };
            let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
            line(columns);
            println!("|-{}-|", sep.join("-|-"));
            cells.iter().for_each(|r| line(r));
            println!("({} linha(s))", rows.len());
        }
    }
}
