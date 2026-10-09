//! Durabilidade e recuperação: WAL, checkpoint, quedas, pool pequeno.

use rustysql::{Database, Options, QueryResult, Value};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

struct Files {
    db: PathBuf,
}

impl Files {
    fn new(name: &str) -> Files {
        let db = std::env::temp_dir().join(format!("rustysql_rec_{name}_{}.db", std::process::id()));
        let f = Files { db };
        let _ = std::fs::remove_file(&f.db);
        let _ = std::fs::remove_file(f.wal());
        f
    }
    fn wal(&self) -> PathBuf {
        PathBuf::from(format!("{}.wal", self.db.display()))
    }
    /// fsync de verdade em cada commit: é isso que está sendo testado
    fn open(&self) -> Database {
        Database::open(&self.db).unwrap()
    }
    fn open_with(&self, opts: Options) -> Database {
        Database::open_with(&self.db, opts).unwrap()
    }
}

fn rows(db: &Database, sql: &str) -> Vec<Vec<Value>> {
    match db.run(sql).unwrap_or_else(|e| panic!("{sql}: {e}")).pop().unwrap() {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("{other:?}"),
    }
}

fn n(db: &Database, sql: &str) -> i64 {
    match &rows(db, sql)[0][0] {
        Value::Int(i) => *i,
        v => panic!("{v:?}"),
    }
}

fn crash(db: Database) {
    db.simulate_crash();
    drop(db); // sem checkpoint final: o arquivo de dados fica como estava no último checkpoint
}

#[test]
fn committed_work_survives_a_crash_including_indexes() {
    let f = Files::new("committed");
    let db = f.open();
    db.run("CREATE TABLE t (id INT PRIMARY KEY, grp INT, txt TEXT)").unwrap();
    db.run("CREATE INDEX t_grp ON t(grp)").unwrap();
    db.run("INSERT INTO t VALUES (1,1,'a'),(2,2,'b'),(3,1,'c'),(4,3,'d')").unwrap();
    db.run("UPDATE t SET txt = 'B!' WHERE id = 2").unwrap();
    db.run("DELETE FROM t WHERE id = 3").unwrap();
    assert!(db.wal_bytes() > 0, "o commit tem que ter ido para o WAL");
    assert!(db.dirty_pages() > 0, "e as páginas ainda não foram para o arquivo de dados");
    crash(db);

    let db = f.open(); // recuperação: refaz o WAL e reconstrói os índices
    assert_eq!(rows(&db, "SELECT id, txt FROM t ORDER BY id"), vec![
        vec![Value::Int(1), Value::Text("a".into())],
        vec![Value::Int(2), Value::Text("B!".into())],
        vec![Value::Int(4), Value::Text("d".into())],
    ]);
    // os índices voltaram e estão sendo usados
    let plan = match db.run("EXPLAIN SELECT id FROM t WHERE grp = 1").unwrap().pop().unwrap() {
        QueryResult::Done(p) => p,
        o => panic!("{o:?}"),
    };
    assert!(plan.contains("t_grp"), "{plan}");
    assert_eq!(rows(&db, "SELECT id FROM t WHERE grp = 1"), vec![vec![Value::Int(1)]]); // o 3 foi apagado
    assert_eq!(rows(&db, "SELECT txt FROM t WHERE id = 2"), vec![vec![Value::Text("B!".into())]]);
    assert!(db.run("INSERT INTO t VALUES (2, 9, 'dup')").is_err(), "a PK continua valendo");
    assert_eq!(db.wal_bytes(), 0, "depois de recuperar, o checkpoint esvazia o WAL");
    assert_eq!(db.dirty_pages(), 0);
}

#[test]
fn an_uncommitted_transaction_is_lost_on_crash() {
    let f = Files::new("uncommitted");
    let db = f.open();
    db.run("CREATE TABLE t (id INT PRIMARY KEY, v INT); INSERT INTO t VALUES (1, 10)").unwrap();
    let mut s = db.session();
    s.execute("BEGIN").unwrap();
    s.execute("INSERT INTO t VALUES (2, 20), (3, 30)").unwrap();
    s.execute("UPDATE t SET v = 99 WHERE id = 1").unwrap();
    // o processo "morre" no meio da transação: nem commit, nem rollback, nem limpeza
    std::mem::forget(s);
    std::mem::forget(db);

    let db = f.open();
    assert_eq!(rows(&db, "SELECT id, v FROM t"), vec![vec![Value::Int(1), Value::Int(10)]]);
    db.run("INSERT INTO t VALUES (2, 22)").unwrap(); // a chave 2 nunca existiu
}

#[test]
fn a_torn_wal_tail_drops_only_the_unfinished_commit() {
    let f = Files::new("torn");
    let db = f.open();
    db.run("CREATE TABLE t (id INT PRIMARY KEY, txt TEXT)").unwrap();
    db.run("INSERT INTO t VALUES (1, 'primeiro')").unwrap();
    let after_first = db.wal_bytes();
    db.run("INSERT INTO t VALUES (2, 'segundo, que vai ser cortado ao meio')").unwrap();
    let after_second = db.wal_bytes();
    crash(db);

    // a queda aconteceu no meio da escrita do segundo commit
    let cut = after_first + (after_second - after_first) / 2;
    std::fs::OpenOptions::new().write(true).open(f.wal()).unwrap().set_len(cut).unwrap();

    let db = f.open();
    assert_eq!(rows(&db, "SELECT id FROM t"), vec![vec![Value::Int(1)]]);
    db.run("INSERT INTO t VALUES (2, 'de novo')").unwrap();
    assert_eq!(n(&db, "SELECT COUNT(*) FROM t"), 2);
}

#[test]
fn a_corrupted_wal_record_is_detected_by_the_checksum() {
    let f = Files::new("crc");
    let db = f.open();
    db.run("CREATE TABLE t (id INT PRIMARY KEY, txt TEXT)").unwrap();
    db.run("INSERT INTO t VALUES (1, 'ok')").unwrap();
    let after_first = db.wal_bytes();
    db.run("INSERT INTO t VALUES (2, 'vai ser corrompido')").unwrap();
    let total = db.wal_bytes();
    crash(db);

    // vira um byte no meio do último registro (tamanho íntegro, conteúdo errado)
    let mut file = std::fs::OpenOptions::new().read(true).write(true).open(f.wal()).unwrap();
    let pos = after_first + (total - after_first) / 2;
    file.seek(SeekFrom::Start(pos)).unwrap();
    let mut b = [0u8; 1];
    file.read_exact(&mut b).unwrap();
    file.seek(SeekFrom::Start(pos)).unwrap();
    file.write_all(&[b[0] ^ 0xFF]).unwrap();
    drop(file);

    let db = f.open();
    assert_eq!(rows(&db, "SELECT id FROM t"), vec![vec![Value::Int(1)]]);
}

#[test]
fn checkpoint_empties_the_wal_and_later_commits_replay_on_top_of_it() {
    let f = Files::new("checkpoint");
    let db = f.open();
    db.run("CREATE TABLE t (id INT PRIMARY KEY, v INT)").unwrap();
    db.run("INSERT INTO t VALUES (1, 1), (2, 2)").unwrap();
    db.run("CHECKPOINT").unwrap();
    assert_eq!((db.wal_bytes(), db.dirty_pages()), (0, 0));

    db.run("INSERT INTO t VALUES (3, 3)").unwrap();
    db.run("UPDATE t SET v = v * 10 WHERE id <= 2").unwrap();
    crash(db);

    let db = f.open();
    assert_eq!(rows(&db, "SELECT id, v FROM t ORDER BY id"), vec![
        vec![Value::Int(1), Value::Int(10)],
        vec![Value::Int(2), Value::Int(20)],
        vec![Value::Int(3), Value::Int(3)],
    ]);
}

#[test]
fn replaying_the_log_twice_gives_the_same_result() {
    // simula uma queda no meio da recuperação/checkpoint: o WAL é reaplicado sobre um
    // arquivo que já tinha parte das páginas (as operações do redo são idempotentes)
    let f = Files::new("idem");
    let db = f.open();
    db.run("CREATE TABLE t (id INT PRIMARY KEY, txt TEXT)").unwrap();
    for i in 0..60 {
        db.run(&format!("INSERT INTO t VALUES ({i}, 'linha {i} com texto para ocupar espaço na página')")).unwrap();
    }
    db.run("DELETE FROM t WHERE id % 3 = 0").unwrap();
    db.run("UPDATE t SET txt = 'alterada' WHERE id % 5 = 0").unwrap();
    let expected = rows(&db, "SELECT id, txt FROM t ORDER BY id");
    let wal = std::fs::read(f.wal()).unwrap();
    crash(db);

    let db = f.open(); // 1ª recuperação (grava as páginas e zera o WAL)
    assert_eq!(rows(&db, "SELECT id, txt FROM t ORDER BY id"), expected);
    drop(db);
    std::fs::write(f.wal(), &wal).unwrap(); // o WAL "voltou": como se o reset não tivesse acontecido
    let db = f.open(); // 2ª recuperação sobre páginas que já têm tudo aplicado
    assert_eq!(rows(&db, "SELECT id, txt FROM t ORDER BY id"), expected);
}

#[test]
fn pages_allocated_by_a_rolled_back_transaction_still_work_after_recovery() {
    // o rollback desfaz as linhas mas não pode "desalocar" as páginas novas já encadeadas:
    // transações seguintes vão escrever nelas, então o WAL precisa registrar a estrutura
    let f = Files::new("structure");
    let db = f.open();
    db.run("CREATE TABLE t (id INT PRIMARY KEY, txt TEXT)").unwrap();
    let mut s = db.session();
    s.execute("BEGIN").unwrap();
    let filler = "x".repeat(200);
    let values: Vec<String> = (0..400).map(|i| format!("({i}, '{filler}')")).collect();
    s.execute(&format!("INSERT INTO t VALUES {}", values.join(","))).unwrap(); // ~20 páginas novas
    s.execute("ROLLBACK").unwrap();
    drop(s);
    assert_eq!(n(&db, "SELECT COUNT(*) FROM t"), 0);

    // agora linhas commitadas vão cair nas páginas que o rollback deixou alocadas
    let values: Vec<String> = (1000..1400).map(|i| format!("({i}, '{filler}')")).collect();
    db.run(&format!("INSERT INTO t VALUES {}", values.join(","))).unwrap();
    crash(db);

    let db = f.open();
    assert_eq!(n(&db, "SELECT COUNT(*) FROM t"), 400);
    assert_eq!(n(&db, "SELECT SUM(id) FROM t"), (1000..1400).sum::<i64>());
    assert_eq!(n(&db, "SELECT COUNT(*) FROM t WHERE id >= 1390"), 10);
}

#[test]
fn ddl_survives_a_crash() {
    let f = Files::new("ddl");
    let db = f.open();
    db.run("CREATE TABLE keep (id INT PRIMARY KEY, v INT); CREATE TABLE gone (id INT PRIMARY KEY)").unwrap();
    db.run("CREATE UNIQUE INDEX keep_v ON keep(v)").unwrap();
    db.run("INSERT INTO keep VALUES (1, 100)").unwrap();
    db.run("DROP TABLE gone").unwrap();
    db.run("CREATE TABLE late (x INT)").unwrap();
    db.run("DROP INDEX keep_v").unwrap();
    db.run("CREATE INDEX keep_v2 ON keep(v)").unwrap();
    crash(db);

    let db = f.open();
    assert_eq!(db.table_names(), ["keep", "late"]);
    let d = db.describe("keep").unwrap();
    let names: Vec<_> = d.indexes.iter().map(|(n, _, _)| n.as_str()).collect();
    assert_eq!(names, ["pk_keep", "keep_v2"]);
    assert!(db.run("SELECT * FROM gone").is_err());
    assert_eq!(rows(&db, "SELECT id FROM keep WHERE v = 100"), vec![vec![Value::Int(1)]]);
    db.run("INSERT INTO keep VALUES (2, 100)").unwrap(); // keep_v (único) foi removido; keep_v2 não é único
}

#[test]
fn a_clean_shutdown_checkpoints_and_leaves_an_empty_wal() {
    let f = Files::new("clean");
    {
        let db = f.open();
        db.run("CREATE TABLE t (id INT PRIMARY KEY)").unwrap();
        db.run("INSERT INTO t VALUES (1), (2), (3)").unwrap();
    } // Drop faz o checkpoint
    assert_eq!(std::fs::metadata(f.wal()).unwrap().len(), 0);
    let db = f.open();
    assert_eq!(n(&db, "SELECT COUNT(*) FROM t"), 3);
    assert!(Path::new(&f.db).exists());
}

#[test]
fn small_buffer_pool_evicts_clean_pages_and_checkpoints_automatically() {
    let f = Files::new("smallpool");
    let opts = Options { pool_pages: 48, sync_commits: false, ..Options::default() };
    let db = f.open_with(opts.clone());
    db.run("CREATE TABLE t (id INT PRIMARY KEY, grp INT, txt TEXT)").unwrap();
    db.run("CREATE INDEX t_grp ON t(grp)").unwrap();
    let filler = "y".repeat(120);
    // ~3000 linhas (~100 páginas de heap + índices) num pool de 48 páginas
    for chunk in 0..30 {
        let values: Vec<String> =
            (0..100).map(|i| format!("({}, {}, '{filler}')", chunk * 100 + i, i % 7)).collect();
        db.run(&format!("INSERT INTO t VALUES {}", values.join(","))).unwrap();
    }
    assert_eq!(n(&db, "SELECT COUNT(*) FROM t"), 3000);
    assert_eq!(n(&db, "SELECT COUNT(*) FROM t WHERE grp = 3"), 420); // 14 por bloco de 100, 30 blocos
    assert_eq!(rows(&db, "SELECT txt FROM t WHERE id = 2999")[0][0], Value::Text("y".repeat(120)));
    assert_eq!(n(&db, "SELECT COUNT(*) FROM t WHERE id >= 1000 AND id < 1100"), 100);
}

#[test]
fn a_transaction_larger_than_the_pool_fails_cleanly_and_the_database_keeps_working() {
    let f = Files::new("toobig");
    let opts = Options { pool_pages: 32, sync_commits: false, ..Options::default() };
    let db = f.open_with(opts);
    db.run("CREATE TABLE t (id INT PRIMARY KEY, txt TEXT)").unwrap();
    db.run("INSERT INTO t VALUES (1, 'antes')").unwrap();

    let filler = "z".repeat(400);
    let values: Vec<String> = (100..1100).map(|i| format!("({i}, '{filler}')")).collect(); // ~100+ páginas
    let err = db.run(&format!("INSERT INTO t VALUES {}", values.join(","))).unwrap_err();
    assert!(err.to_string().contains("buffer pool"), "{err}");

    assert_eq!(n(&db, "SELECT COUNT(*) FROM t"), 1, "o comando que falhou foi desfeito por completo");
    db.run("INSERT INTO t VALUES (2, 'depois')").unwrap();
    assert_eq!(n(&db, "SELECT COUNT(*) FROM t"), 2);
}
