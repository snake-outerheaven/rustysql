//! rustysql studio: aplicativo desktop de exemplo, feito com Tauri 2, para gerenciar um banco
//! rustysql. O backend (este arquivo) é um cliente comum da biblioteca: abre um [`Database`],
//! mantém uma [`Session`] para o console SQL e expõe tudo à interface por comandos Tauri.
//!
//! A interface (`ui/`) é HTML + JavaScript puro, sem etapa de build.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustysql::{Database, Error, Priority, QueryResult, Session, Value};
use serde::Serialize;
use serde_json::{Value as Json, json};

const DEMO_SQL: &str = include_str!("../../demo.sql");

/// Estado compartilhado: o banco aberto e a sessão do console (que mantém `BEGIN` aberto
/// entre um comando e outro).
#[derive(Default)]
struct AppState {
    db: Mutex<Option<Database>>,
    console: Mutex<Option<Session>>,
    path: Mutex<String>,
}

type Shared = Arc<AppState>;
type Reply<T> = Result<T, String>;

fn current(state: &AppState) -> Reply<Database> {
    state.db.lock().unwrap().clone().ok_or_else(|| "nenhum banco aberto".to_string())
}

fn to_json(v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Int(i) => json!(i),
        Value::Float(x) => json!(x),
        Value::Text(s) => json!(s),
        Value::Bool(b) => json!(b),
    }
}

fn result_json(r: QueryResult) -> Json {
    match r {
        QueryResult::Rows { columns, rows } => json!({
            "kind": "rows",
            "columns": columns,
            "rows": rows.iter().map(|r| r.iter().map(to_json).collect::<Vec<_>>()).collect::<Vec<_>>(),
        }),
        QueryResult::Affected(n) => json!({ "kind": "affected", "n": n }),
        QueryResult::Done(m) => json!({ "kind": "done", "message": m }),
    }
}

fn err_kind(e: &Error) -> &'static str {
    match e {
        Error::Deadlock | Error::LockTimeout | Error::QueueTimeout => "transient",
        _ => "error",
    }
}

/// Roda trabalho bloqueante fora da thread da interface.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Reply<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())
}

fn snapshot(db: &Database, path: &str) -> Json {
    let tables: Vec<Json> = db
        .table_names()
        .into_iter()
        .filter_map(|name| {
            let d = db.describe(&name)?;
            let rows = db
                .run(&format!("SELECT COUNT(*) FROM {name}"))
                .ok()
                .and_then(|mut r| match r.pop() {
                    Some(QueryResult::Rows { rows, .. }) => rows.first().and_then(|r| r.first()).map(to_json),
                    _ => None,
                })
                .unwrap_or(Json::Null);
            Some(json!({
                "name": name,
                "rows": rows,
                "columns": d.columns.iter().map(|c| json!({
                    "name": c.name,
                    "type": format!("{:?}", c.ty).to_uppercase(),
                    "not_null": c.not_null,
                    "pk": c.primary_key,
                })).collect::<Vec<_>>(),
                "indexes": d.indexes.iter().map(|(n, c, u)| json!({"name": n, "column": c, "unique": u})).collect::<Vec<_>>(),
            }))
        })
        .collect();
    json!({ "path": path, "tables": tables })
}

#[tauri::command]
async fn open_db(state: tauri::State<'_, Shared>, path: String) -> Reply<Json> {
    let st = state.inner().clone();
    blocking(move || {
        let path = path.trim().to_string();
        if path.is_empty() {
            return Err("informe o caminho do arquivo .db".to_string());
        }
        // solta o banco anterior antes de abrir outro (faz o checkpoint final)
        *st.console.lock().unwrap() = None;
        *st.db.lock().unwrap() = None;
        let db = Database::open(&path).map_err(|e| e.to_string())?;
        *st.console.lock().unwrap() = Some(db.session());
        let info = snapshot(&db, &path);
        *st.path.lock().unwrap() = path;
        *st.db.lock().unwrap() = Some(db);
        Ok(info)
    })
    .await?
}

#[tauri::command]
async fn close_db(state: tauri::State<'_, Shared>) -> Reply<()> {
    let st = state.inner().clone();
    blocking(move || {
        *st.console.lock().unwrap() = None;
        *st.db.lock().unwrap() = None;
    })
    .await
}

#[tauri::command]
async fn schema(state: tauri::State<'_, Shared>) -> Reply<Json> {
    let st = state.inner().clone();
    blocking(move || {
        let db = current(&st)?;
        let path = st.path.lock().unwrap().clone();
        Ok(snapshot(&db, &path))
    })
    .await?
}

/// Executa SQL na sessão do console. Devolve um item por comando, o tempo gasto e se há
/// transação aberta; em caso de erro, o erro vem no mesmo envelope (a transação continua
/// aberta, como no REPL).
#[tauri::command]
async fn run_sql(state: tauri::State<'_, Shared>, sql: String, priority: String) -> Reply<Json> {
    let st = state.inner().clone();
    blocking(move || {
        let mut guard = st.console.lock().unwrap();
        let s = guard.as_mut().ok_or("nenhum banco aberto")?;
        s.set_priority(Priority::parse(&priority).unwrap_or_default());
        let t = Instant::now();
        let out = s.execute(&sql);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let in_txn = s.in_transaction();
        Ok(match out {
            Ok(rs) => json!({ "ok": true, "ms": ms, "in_txn": in_txn,
                              "results": rs.into_iter().map(result_json).collect::<Vec<_>>() }),
            Err(e) => json!({ "ok": false, "ms": ms, "in_txn": in_txn,
                              "kind": err_kind(&e), "error": e.to_string() }),
        })
    })
    .await?
}

#[tauri::command]
async fn browse(state: tauri::State<'_, Shared>, table: String, limit: u32, offset: u32) -> Reply<Json> {
    let st = state.inner().clone();
    blocking(move || {
        let db = current(&st)?;
        // o nome vem da interface: só aceita tabelas que existem
        if !db.table_names().contains(&table) {
            return Err(format!("tabela inexistente: {table}"));
        }
        let rs = db
            .run(&format!("SELECT * FROM {table} LIMIT {} OFFSET {}", limit.min(1000), offset))
            .map_err(|e| e.to_string())?;
        rs.into_iter().next().map(result_json).ok_or_else(|| "sem resultado".to_string())
    })
    .await?
}

#[tauri::command]
async fn explain(state: tauri::State<'_, Shared>, sql: String) -> Reply<String> {
    let st = state.inner().clone();
    blocking(move || {
        let db = current(&st)?;
        let rs = db.run(&format!("EXPLAIN {}", sql.trim().trim_end_matches(';'))).map_err(|e| e.to_string())?;
        Ok(rs
            .into_iter()
            .map(|r| match r {
                QueryResult::Done(m) => m,
                other => result_json(other).to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"))
    })
    .await?
}

#[tauri::command]
async fn load_demo(state: tauri::State<'_, Shared>) -> Reply<Json> {
    let st = state.inner().clone();
    blocking(move || {
        let db = current(&st)?;
        let t = Instant::now();
        db.run(DEMO_SQL).map_err(|e| e.to_string())?;
        let path = st.path.lock().unwrap().clone();
        let mut j = snapshot(&db, &path);
        j["ms"] = json!(t.elapsed().as_secs_f64() * 1000.0);
        Ok(j)
    })
    .await?
}

#[tauri::command]
async fn checkpoint(state: tauri::State<'_, Shared>) -> Reply<f64> {
    let st = state.inner().clone();
    blocking(move || {
        let db = current(&st)?;
        let t = Instant::now();
        db.checkpoint().map_err(|e| e.to_string())?;
        Ok(t.elapsed().as_secs_f64() * 1000.0)
    })
    .await?
}

/// Painel de monitoramento: tudo que a biblioteca expõe sobre o estado interno.
#[tauri::command]
async fn monitor(state: tauri::State<'_, Shared>) -> Reply<Json> {
    let st = state.inner().clone();
    blocking(move || {
        let db = current(&st)?;
        let s = db.scheduler_stats();
        Ok(json!({
            "dirty_pages": db.dirty_pages(),
            "wal_bytes": db.wal_bytes(),
            "locks": db.lock_snapshot(),
            "in_txn": st.console.lock().unwrap().as_ref().is_some_and(|s| s.in_transaction()),
            "sched": {
                "running": s.running, "queued": s.queued,
                "admitted": s.admitted, "admitted_in_txn": s.admitted_in_txn,
                "resumed": s.resumed, "waited": s.waited,
                "max_wait_ms": s.max_wait.as_secs_f64() * 1000.0,
            },
        }))
    })
    .await?
}

#[derive(Serialize)]
struct Worker {
    id: usize,
    priority: &'static str,
    ms: f64,
    transfers: usize,
    retries: usize,
}

/// Laboratório de concorrência: `workers` threads fazem transferências entre contas dentro de
/// transações. O primeiro trabalhador roda em prioridade ALTA, o último em BAIXA, os demais em
/// NORMAL. Deadlocks são tratados repetindo a transação. No fim confere o invariante: a soma
/// dos saldos não muda.
#[tauri::command]
async fn run_lab(state: tauri::State<'_, Shared>, workers: usize, ops: usize) -> Reply<Json> {
    let st = state.inner().clone();
    blocking(move || {
        let db = current(&st)?;
        let (workers, ops) = (workers.clamp(2, 16), ops.clamp(1, 500));
        const ACCOUNTS: i64 = 10;
        if !db.table_names().iter().any(|t| t == "lab_conta") {
            db.run("CREATE TABLE lab_conta (id INT PRIMARY KEY, saldo INT)").map_err(|e| e.to_string())?;
        }
        db.run("DELETE FROM lab_conta").map_err(|e| e.to_string())?;
        for id in 0..ACCOUNTS {
            db.run(&format!("INSERT INTO lab_conta VALUES ({id}, 1000)")).map_err(|e| e.to_string())?;
        }
        let before = ACCOUNTS * 1000;
        let t0 = Instant::now();

        let handles: Vec<_> = (0..workers)
            .map(|w| {
                let db = db.clone();
                let prio = if w == 0 { Priority::High } else if w == workers - 1 { Priority::Low } else { Priority::Normal };
                std::thread::spawn(move || -> Result<Worker, String> {
                    let mut s = db.session();
                    s.set_priority(prio);
                    let t = Instant::now();
                    let (mut done, mut retries) = (0usize, 0usize);
                    let mut seed = 0x9E3779B97F4A7C15u64 ^ (w as u64 + 1).wrapping_mul(0xBF58476D1CE4E5B9);
                    let mut next = move || {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        seed
                    };
                    while done < ops {
                        let from = (next() % ACCOUNTS as u64) as i64;
                        let to = (from + 1 + (next() % (ACCOUNTS as u64 - 1)) as i64) % ACCOUNTS;
                        let amount = 1 + (next() % 20) as i64;
                        let attempt = (|| -> rustysql::Result<()> {
                            s.execute("BEGIN")?;
                            let r = (|| {
                                s.execute(&format!("UPDATE lab_conta SET saldo = saldo - {amount} WHERE id = {from}"))?;
                                s.execute(&format!("UPDATE lab_conta SET saldo = saldo + {amount} WHERE id = {to}"))?;
                                s.execute("COMMIT")
                            })();
                            if r.is_err() {
                                let _ = s.execute("ROLLBACK");
                            }
                            r.map(|_| ())
                        })();
                        match attempt {
                            Ok(()) => done += 1,
                            Err(Error::Deadlock | Error::LockTimeout | Error::QueueTimeout) => {
                                retries += 1;
                                std::thread::sleep(Duration::from_millis(1 + (retries as u64 % 5)));
                            }
                            Err(e) => return Err(e.to_string()),
                        }
                    }
                    Ok(Worker {
                        id: w,
                        priority: match prio { Priority::High => "HIGH", Priority::Normal => "NORMAL", Priority::Low => "LOW" },
                        ms: t.elapsed().as_secs_f64() * 1000.0,
                        transfers: done,
                        retries,
                    })
                })
            })
            .collect();

        let mut out = Vec::new();
        for h in handles {
            out.push(h.join().map_err(|_| "thread do laboratório entrou em pânico".to_string())??);
        }
        let total_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let sum = match db.run("SELECT SUM(saldo) FROM lab_conta").map_err(|e| e.to_string())?.pop() {
            Some(QueryResult::Rows { rows, .. }) => match rows.first().and_then(|r| r.first()) {
                Some(Value::Int(i)) => *i,
                _ => -1,
            },
            _ => -1,
        };
        Ok(json!({ "workers": out, "total_ms": total_ms, "sum_before": before, "sum_after": sum,
                   "consistent": sum == before }))
    })
    .await?
}

fn main() {
    tauri::Builder::default()
        .manage(Shared::default())
        .invoke_handler(tauri::generate_handler![
            open_db, close_db, schema, run_sql, browse, explain, load_demo, checkpoint, monitor, run_lab
        ])
        .run(tauri::generate_context!())
        .expect("erro ao iniciar o rustysql studio");
}
