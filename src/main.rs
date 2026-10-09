use rustysql::{Database, QueryResult};
use std::io::{self, BufRead, Write};

const HELP: &str = "\
Comandos:
  .tables          lista as tabelas
  .schema <tabela> mostra as colunas
  .help            esta ajuda
  .exit            sai
SQL (termine com ';'): CREATE TABLE, DROP TABLE, INSERT, SELECT, UPDATE, DELETE";

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "rustysql.db".to_string());
    let mut db = match Database::open(&path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("não consegui abrir '{path}': {e}");
            std::process::exit(1);
        }
    };

    // modo script: `rustysql <db> <arquivo.sql>` executa e ecoa cada comando
    if let Some(script) = std::env::args().nth(2) {
        let sql = std::fs::read_to_string(&script).unwrap_or_else(|e| {
            eprintln!("não consegui ler '{script}': {e}");
            std::process::exit(1);
        });
        for stmt in sql.split_inclusive(';').map(str::trim).filter(|s| !s.is_empty()) {
            let shown: String = stmt.chars().take(110).collect();
            println!("rustysql> {shown}{}", if shown.len() < stmt.len() { " ..." } else { "" });
            let start = std::time::Instant::now();
            match db.run(stmt) {
                Ok(results) => results.iter().for_each(print_result),
                Err(e) => println!("{e}"),
            }
            println!("[{:.2?}]
", start.elapsed());
        }
        return;
    }

    println!("rustysql — {path}  (.help para ajuda)");

    let stdin = io::stdin();
    let mut buffer = String::new();
    loop {
        print!("{}", if buffer.is_empty() { "rustysql> " } else { "       ...> " });
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
                ".schema" => match parts.next().and_then(|t| db.table_meta(&t.to_lowercase())) {
                    Some(meta) => {
                        for c in &meta.schema.columns {
                            let mut flags = String::new();
                            if c.primary_key { flags.push_str(" PRIMARY KEY"); }
                            else if c.not_null { flags.push_str(" NOT NULL"); }
                            println!("  {} {}{}", c.name, c.ty.name(), flags);
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
        match db.run(&buffer) {
            Ok(results) => results.iter().for_each(print_result),
            Err(e) => println!("{e}"),
        }
        buffer.clear();
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
