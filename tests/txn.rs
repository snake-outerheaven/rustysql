//! Transações, isolamento (strict 2PL) e concorrência no nível do banco.

use rustysql::{Database, Error, Options, QueryResult, Session, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

fn open(name: &str) -> Database {
    let p: PathBuf = std::env::temp_dir().join(format!("rustysql_txn_{name}_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_file(format!("{}.wal", p.display()));
    let opts = Options { sync_commits: false, lock_timeout: Duration::from_secs(10), ..Options::default() };
    Database::open_with(p, opts).unwrap()
}

fn query(s: &mut Session, sql: &str) -> Vec<Vec<Value>> {
    match s.execute(sql).unwrap_or_else(|e| panic!("{sql}: {e}")).pop().unwrap() {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("esperava linhas, veio {other:?}"),
    }
}

fn scalar(s: &mut Session, sql: &str) -> Value {
    query(s, sql).remove(0).remove(0)
}

fn int(n: i64) -> Value {
    Value::Int(n)
}

#[test]
fn rollback_and_commit() {
    let db = open("basic");
    let mut s = db.session();
    s.execute("CREATE TABLE t (id INT PRIMARY KEY, v INT); INSERT INTO t VALUES (1, 10), (2, 20)").unwrap();

    s.execute("BEGIN").unwrap();
    s.execute("INSERT INTO t VALUES (3, 30)").unwrap();
    s.execute("UPDATE t SET v = v + 1 WHERE id = 1").unwrap();
    s.execute("DELETE FROM t WHERE id = 2").unwrap();
    // a própria transação enxerga as suas mudanças...
    assert_eq!(scalar(&mut s, "SELECT COUNT(*) FROM t"), int(2));
    assert_eq!(scalar(&mut s, "SELECT v FROM t WHERE id = 1"), int(11));
    s.execute("ROLLBACK").unwrap();
    // ...e o rollback devolve tudo, índice incluído
    assert_eq!(scalar(&mut s, "SELECT COUNT(*) FROM t"), int(2));
    assert_eq!(scalar(&mut s, "SELECT v FROM t WHERE id = 1"), int(10));
    assert_eq!(scalar(&mut s, "SELECT v FROM t WHERE id = 2"), int(20));
    assert!(query(&mut s, "SELECT v FROM t WHERE id = 3").is_empty());
    s.execute("INSERT INTO t VALUES (3, 33)").unwrap(); // a chave 3 voltou a ficar livre

    s.execute("BEGIN; UPDATE t SET v = 0; COMMIT").unwrap();
    assert_eq!(scalar(&mut s, "SELECT SUM(v) FROM t"), int(0));
    assert!(s.execute("COMMIT").is_err(), "COMMIT sem BEGIN");
    assert!(s.execute("BEGIN; BEGIN").is_err(), "BEGIN aninhado");
    s.execute("ROLLBACK").unwrap();
}

#[test]
fn a_failing_statement_does_not_abort_the_transaction() {
    let db = open("stmt");
    let mut s = db.session();
    s.execute("CREATE TABLE t (id INT PRIMARY KEY, v INT)").unwrap();
    s.execute("BEGIN").unwrap();
    s.execute("INSERT INTO t VALUES (1, 1)").unwrap();
    assert!(s.execute("INSERT INTO t VALUES (2, 2), (1, 99)").is_err()); // desfaz só este comando
    assert!(s.execute("SELECT nope FROM t").is_err());
    s.execute("INSERT INTO t VALUES (3, 3)").unwrap();
    assert!(s.in_transaction());
    s.execute("COMMIT").unwrap();
    assert_eq!(query(&mut s, "SELECT id FROM t ORDER BY id"), vec![vec![int(1)], vec![int(3)]]);
    assert!(s.execute("BEGIN; CREATE TABLE x (a INT)").is_err(), "DDL não entra em transação explícita");
    s.execute("ROLLBACK").unwrap();
}

#[test]
fn dropping_a_session_rolls_back() {
    let db = open("drop");
    db.run("CREATE TABLE t (id INT PRIMARY KEY)").unwrap();
    {
        let mut s = db.session();
        s.execute("BEGIN; INSERT INTO t VALUES (1), (2), (3)").unwrap();
    } // sai de escopo sem COMMIT
    assert_eq!(db.run("SELECT COUNT(*) FROM t").unwrap().len(), 1);
    let mut s = db.session();
    assert_eq!(scalar(&mut s, "SELECT COUNT(*) FROM t"), int(0));
    s.execute("INSERT INTO t VALUES (1)").unwrap(); // locks foram soltos
}

#[test]
fn readers_wait_for_writers_and_never_see_dirty_data() {
    let db = open("dirty");
    let mut writer = db.session();
    writer.execute("CREATE TABLE t (id INT PRIMARY KEY, v INT); INSERT INTO t VALUES (1, 100)").unwrap();
    writer.execute("BEGIN; UPDATE t SET v = 999 WHERE id = 1; INSERT INTO t VALUES (2, 2)").unwrap();

    let (tx, rx) = mpsc::channel();
    let reader_db = db.clone();
    let reader = thread::spawn(move || {
        let mut s = reader_db.session();
        let v = query(&mut s, "SELECT v FROM t WHERE id = 1");
        let n = scalar(&mut s, "SELECT COUNT(*) FROM t");
        tx.send((v, n)).unwrap();
    });
    // enquanto o escritor segura o lock exclusivo, o leitor não termina
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "o leitor furou o lock");

    writer.execute("ROLLBACK").unwrap();
    let (v, n) = rx.recv_timeout(Duration::from_secs(5)).expect("o leitor deveria acordar");
    assert_eq!(v, vec![vec![int(100)]], "dirty read: viu o valor não commitado");
    assert_eq!(n, int(1));
    reader.join().unwrap();

    // e depois de um COMMIT o leitor vê o valor novo
    writer.execute("BEGIN; UPDATE t SET v = 7 WHERE id = 1").unwrap();
    let reader_db = db.clone();
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        tx.send(scalar(&mut reader_db.session(), "SELECT v FROM t WHERE id = 1")).unwrap();
    });
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    writer.execute("COMMIT").unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), int(7));
    reader.join().unwrap();
}

#[test]
fn readers_share_the_lock() {
    let db = open("shared");
    let mut a = db.session();
    a.execute("CREATE TABLE t (id INT PRIMARY KEY); INSERT INTO t VALUES (1)").unwrap();
    a.execute("BEGIN").unwrap();
    assert_eq!(scalar(&mut a, "SELECT COUNT(*) FROM t"), int(1)); // A segura lock compartilhado

    let db2 = db.clone();
    let b = thread::spawn(move || scalar(&mut db2.session(), "SELECT COUNT(*) FROM t"));
    assert_eq!(b.join().unwrap(), int(1)); // B lê sem esperar A
    a.execute("COMMIT").unwrap();
}

#[test]
fn deadlock_is_detected_and_one_victim_is_chosen() {
    let db = open("deadlock");
    db.run("CREATE TABLE a (id INT PRIMARY KEY, v INT); CREATE TABLE b (id INT PRIMARY KEY, v INT)").unwrap();
    db.run("INSERT INTO a VALUES (1, 0); INSERT INTO b VALUES (1, 0)").unwrap();

    let barrier = Arc::new(Barrier::new(2));
    let spawn = |first: &'static str, second: &'static str| {
        let (db, barrier) = (db.clone(), barrier.clone());
        thread::spawn(move || -> Result<(), Error> {
            let mut s = db.session();
            s.execute("BEGIN")?;
            s.execute(&format!("UPDATE {first} SET v = v + 1"))?;
            barrier.wait(); // os dois já seguram um lock
            s.execute(&format!("UPDATE {second} SET v = v + 1"))?;
            s.execute("COMMIT")?;
            Ok(())
        })
    };
    let (t1, t2) = (spawn("a", "b"), spawn("b", "a"));
    let results = [t1.join().unwrap(), t2.join().unwrap()];

    let deadlocks = results.iter().filter(|r| matches!(r, Err(Error::Deadlock))).count();
    let oks = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!((deadlocks, oks), (1, 1), "exatamente uma vítima: {results:?}");

    // a vítima desfez tudo; o vencedor aplicou as suas duas atualizações
    let mut s = db.session();
    assert_eq!(scalar(&mut s, "SELECT v FROM a"), int(1));
    assert_eq!(scalar(&mut s, "SELECT v FROM b"), int(1));
}

/// Transfere `amount` entre duas contas (em tabelas escolhidas pelo chamador); tenta de
/// novo quando é escolhido como vítima de deadlock.
fn transfer(db: &Database, from: &str, to: &str, amount: i64, retries: &AtomicUsize) {
    let mut s = db.session();
    let mut attempt = 0u32;
    loop {
        let result = (|| -> Result<(), Error> {
            s.execute("BEGIN")?;
            s.execute(&format!("UPDATE {from} SET bal = bal - {amount} WHERE id = 1"))?;
            s.execute(&format!("UPDATE {to} SET bal = bal + {amount} WHERE id = 1"))?;
            s.execute("COMMIT")?;
            Ok(())
        })();
        match result {
            Ok(()) => return,
            Err(Error::Deadlock | Error::LockTimeout) => {
                let n = retries.fetch_add(1, Ordering::SeqCst) as u64;
                attempt += 1;
                // recuo exponencial com jitter: dobra a cada colisão (até ~25 ms)
                let cap = 100u64 << attempt.min(8);
                thread::sleep(Duration::from_micros(cap / 2 + (n * 7919) % (cap / 2 + 1)));
            }
            Err(e) => panic!("erro inesperado: {e}"),
        }
    }
}

#[test]
fn concurrent_transfers_conserve_money_and_never_expose_partial_state() {
    let db = open("bank");
    db.run(
        "CREATE TABLE checking (id INT PRIMARY KEY, bal INT);
         CREATE TABLE savings (id INT PRIMARY KEY, bal INT);
         INSERT INTO checking VALUES (1, 1000);
         INSERT INTO savings VALUES (1, 1000);",
    )
    .unwrap();

    let retries = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // auditores: a soma só é consistente se cada transferência for atômica e isolada
    let auditors: Vec<_> = (0..2)
        .map(|_| {
            let (db, stop) = (db.clone(), stop.clone());
            thread::spawn(move || {
                let mut s = db.session();
                let mut checks = 0;
                while !stop.load(Ordering::SeqCst) {
                    let sql = "SELECT c.bal + s.bal FROM checking c JOIN savings s ON c.id = s.id";
                    match s.execute(sql) {
                        Ok(mut r) => {
                            let QueryResult::Rows { rows, .. } = r.pop().unwrap() else { panic!() };
                            assert_eq!(rows, vec![vec![int(2000)]], "o auditor viu uma transferência pela metade");
                            checks += 1;
                        }
                        // um leitor também pode ser a vítima de um deadlock: é só tentar de novo
                        Err(Error::Deadlock | Error::LockTimeout) => {}
                        Err(e) => panic!("{e}"),
                    }
                }
                checks
            })
        })
        .collect();

    let workers: Vec<_> = (0..6)
        .map(|w| {
            let (db, retries) = (db.clone(), retries.clone());
            thread::spawn(move || {
                for n in 0..40 {
                    // metade das threads inverte a ordem dos locks: força deadlocks reais
                    let (from, to) = if (w + n) % 2 == 0 { ("checking", "savings") } else { ("savings", "checking") };
                    transfer(&db, from, to, 1 + (n % 5) as i64, &retries);
                }
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
    stop.store(true, Ordering::SeqCst);
    let checks: i32 = auditors.into_iter().map(|a| a.join().unwrap()).sum();
    assert!(checks > 0);

    let mut s = db.session();
    assert_eq!(
        scalar(&mut s, "SELECT c.bal + s.bal FROM checking c JOIN savings s ON c.id = s.id"),
        int(2000)
    );
    eprintln!("deadlocks resolvidos por retry: {}, auditorias: {checks}", retries.load(Ordering::SeqCst));
}

#[test]
fn parallel_inserts_into_same_and_different_tables() {
    let db = open("parallel");
    db.run("CREATE TABLE shared (id INT PRIMARY KEY, who INT)").unwrap();
    for t in 0..4 {
        db.run(&format!("CREATE TABLE own{t} (id INT PRIMARY KEY, txt TEXT)")).unwrap();
    }
    let handles: Vec<_> = (0..4)
        .map(|t| {
            let db = db.clone();
            thread::spawn(move || {
                let mut s = db.session();
                for n in 0..150 {
                    s.execute(&format!("INSERT INTO shared VALUES ({}, {t})", t * 1000 + n)).unwrap();
                    s.execute(&format!("INSERT INTO own{t} VALUES ({n}, 'linha {n} da thread {t}')")).unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let mut s = db.session();
    assert_eq!(scalar(&mut s, "SELECT COUNT(*) FROM shared"), int(600));
    for t in 0..4 {
        assert_eq!(scalar(&mut s, &format!("SELECT COUNT(*) FROM own{t}")), int(150));
        assert_eq!(scalar(&mut s, &format!("SELECT COUNT(*) FROM shared WHERE who = {t}")), int(150));
    }
    // a unicidade da PK vale mesmo sob concorrência: 4 threads brigando pela mesma chave
    let winners = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let (db, winners) = (db.clone(), winners.clone());
            thread::spawn(move || {
                if db.run("INSERT INTO shared VALUES (777777, 9)").is_ok() {
                    winners.fetch_add(1, Ordering::SeqCst);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(winners.load(Ordering::SeqCst), 1);
    assert_eq!(scalar(&mut s, "SELECT COUNT(*) FROM shared WHERE id = 777777"), int(1));
}
