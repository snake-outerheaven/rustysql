//! Gerenciador de prioridade: ordem de admissão, aging, limite de concorrência e a
//! integração com locks (vaga cedida ao bloquear, vítima de deadlock por prioridade).

use rustysql::sched::{Admit, Priority, SchedConfig, Scheduler};
use rustysql::{Database, Error, Options, QueryResult, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

fn cfg(max_running: usize, aging_ms: u64) -> SchedConfig {
    SchedConfig {
        max_running,
        aging_step: Duration::from_millis(aging_ms),
        queue_timeout: Duration::from_secs(10),
    }
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "tempo esgotado esperando: {what}");
        thread::sleep(Duration::from_millis(2));
    }
}

/// Enfileira um comando numa thread e devolve o handle; o nome sai no canal quando ele for admitido.
fn enqueue(
    sched: &Arc<Scheduler>,
    why: Admit,
    label: &'static str,
    out: &mpsc::Sender<&'static str>,
) -> thread::JoinHandle<()> {
    let (sched, out) = (sched.clone(), out.clone());
    thread::spawn(move || {
        let permit = sched.admit(why).unwrap();
        out.send(label).unwrap();
        thread::sleep(Duration::from_millis(15)); // segura a vaga um instante
        drop(permit);
    })
}

/// Coloca os comandos na fila *na ordem dada* (esperando cada um chegar) e então libera a vaga.
fn run_queue(sched: &Arc<Scheduler>, order: &[(Admit, &'static str)], pause_ms: u64) -> Vec<&'static str> {
    let blocker = sched.admit(Admit::New(Priority::Normal)).unwrap();
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::new();
    for (i, (why, label)) in order.iter().enumerate() {
        handles.push(enqueue(sched, *why, label, &tx));
        wait_until("comando entrar na fila", || sched.stats().queued == i + 1);
    }
    thread::sleep(Duration::from_millis(pause_ms));
    drop(blocker);
    let got: Vec<_> = (0..order.len()).map(|_| rx.recv_timeout(Duration::from_secs(10)).unwrap()).collect();
    handles.into_iter().for_each(|h| h.join().unwrap());
    got
}

use Priority::{High, Low, Normal};

#[test]
fn higher_priority_enters_first_and_fifo_within_a_level() {
    let sched = Scheduler::new(cfg(1, 3_600_000)); // aging desligado na prática
    let got = run_queue(
        &sched,
        &[
            (Admit::New(Low), "low"),
            (Admit::New(Normal), "normal1"),
            (Admit::New(High), "high1"),
            (Admit::New(Normal), "normal2"),
            (Admit::New(High), "high2"),
        ],
        0,
    );
    assert_eq!(got, ["high1", "high2", "normal1", "normal2", "low"]);
}

#[test]
fn transaction_holders_and_resumed_statements_outrank_user_priorities() {
    let sched = Scheduler::new(cfg(1, 3_600_000));
    let got = run_queue(
        &sched,
        &[
            (Admit::New(High), "high"),
            (Admit::InTransaction, "in-txn"),
            (Admit::New(Normal), "normal"),
            (Admit::Resume, "resume"),
        ],
        0,
    );
    assert_eq!(got, ["resume", "in-txn", "high", "normal"]);
}

#[test]
fn aging_lets_a_long_waiting_low_overtake_a_fresh_high() {
    // com aging curto, esperar ~4,5 passos vale mais que os 2 níveis de diferença
    let sched = Scheduler::new(cfg(1, 100));
    let blocker = sched.admit(Admit::New(Normal)).unwrap();
    let (tx, rx) = mpsc::channel();
    let h1 = enqueue(&sched, Admit::New(Low), "low (velho)", &tx);
    wait_until("low na fila", || sched.stats().queued == 1);
    thread::sleep(Duration::from_millis(450));
    let h2 = enqueue(&sched, Admit::New(High), "high (novo)", &tx);
    wait_until("high na fila", || sched.stats().queued == 2);
    drop(blocker);
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "low (velho)");
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "high (novo)");
    h1.join().unwrap();
    h2.join().unwrap();

    // controle: sem tempo suficiente, a prioridade manda (o mesmo cenário com pausa curta)
    let sched = Scheduler::new(cfg(1, 5_000));
    let got = run_queue(&sched, &[(Admit::New(Low), "low"), (Admit::New(High), "high")], 50);
    assert_eq!(got, ["high", "low"]);
}

#[test]
fn never_runs_more_than_max_running() {
    let sched = Scheduler::new(cfg(3, 3_600_000));
    let (live, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let handles: Vec<_> = (0..12)
        .map(|i| {
            let (sched, live, peak) = (sched.clone(), live.clone(), peak.clone());
            thread::spawn(move || {
                let prio = [Low, Normal, High][i % 3];
                for _ in 0..15 {
                    let _permit = sched.admit(Admit::New(prio)).unwrap();
                    let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    assert!(now <= 3, "{now} comandos ao mesmo tempo");
                    thread::sleep(Duration::from_millis(1));
                    live.fetch_sub(1, Ordering::SeqCst);
                }
            })
        })
        .collect();
    handles.into_iter().for_each(|h| h.join().unwrap());
    assert_eq!(peak.load(Ordering::SeqCst), 3, "o limite deveria ter sido atingido");
    let stats = sched.stats();
    assert_eq!((stats.running, stats.queued), (0, 0));
    assert_eq!(stats.admitted, [60, 60, 60]);
    assert!(stats.waited > 0 && stats.max_wait > Duration::ZERO);
}

#[test]
fn queue_timeout_gives_up_and_does_not_block_those_behind() {
    let sched = Scheduler::new(SchedConfig {
        max_running: 1,
        aging_step: Duration::from_secs(3600),
        queue_timeout: Duration::from_millis(150),
    });
    let blocker = sched.admit(Admit::New(Normal)).unwrap();

    // este desiste (e deixa uma entrada "cancelada" no topo da fila)
    let start = Instant::now();
    assert!(matches!(sched.admit(Admit::New(High)), Err(Error::QueueTimeout)));
    assert!(start.elapsed() >= Duration::from_millis(140));
    assert_eq!(sched.stats().queued, 0, "a desistência não conta como fila");

    // quem chega depois não fica preso atrás da entrada cancelada
    let s2 = sched.clone();
    let waiter = thread::spawn(move || s2.admit(Admit::New(Low)).map(|_| ()));
    wait_until("waiter na fila", || sched.stats().queued == 1);
    drop(blocker);
    assert!(waiter.join().unwrap().is_ok());
}

#[test]
fn yielding_a_permit_frees_the_slot_and_resume_waits_for_one() {
    let sched = Scheduler::new(cfg(1, 3_600_000));
    let mut a = sched.admit(Admit::New(Normal)).unwrap();
    a.yield_slot();
    assert_eq!(sched.stats().running, 0);
    let b = sched.admit(Admit::New(Low)).unwrap(); // a vaga cedida foi aproveitada
    assert_eq!(sched.stats().running, 1);

    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        a.resume().unwrap(); // precisa esperar `b` sair
        tx.send(()).unwrap();
    });
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    drop(b);
    rx.recv_timeout(Duration::from_secs(5)).expect("resume deveria acordar");
    handle.join().unwrap();
    assert_eq!(sched.stats().resumed, 1);
}

// ---- integração com o motor ----------------------------------------------------

fn open(name: &str, max_running: usize) -> Database {
    let p = std::env::temp_dir().join(format!("rustysql_sched_{name}_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_file(format!("{}.wal", p.display()));
    let opts = Options {
        sync_commits: false,
        lock_timeout: Duration::from_secs(10),
        sched: cfg(max_running, 3_600_000),
        ..Options::default()
    };
    Database::open_with(p, opts).unwrap()
}

fn count(db: &Database, sql: &str) -> Value {
    match db.run(sql).unwrap().pop().unwrap() {
        QueryResult::Rows { mut rows, .. } => rows.remove(0).remove(0),
        other => panic!("{other:?}"),
    }
}

#[test]
fn set_priority_changes_how_statements_are_admitted() {
    let db = open("setprio", 8);
    let mut s = db.session();
    s.execute("CREATE TABLE t (id INT PRIMARY KEY)").unwrap();
    assert_eq!(s.priority(), Normal);

    s.execute("SET PRIORITY HIGH").unwrap();
    assert_eq!(s.priority(), High);
    s.execute("INSERT INTO t VALUES (1)").unwrap();
    s.execute("SET PRIORITY low").unwrap();
    s.execute("INSERT INTO t VALUES (2)").unwrap();
    assert!(s.execute("SET PRIORITY urgentissimo").is_err());

    let st = db.scheduler_stats();
    assert_eq!(st.admitted[High as usize], 1);
    assert_eq!(st.admitted[Low as usize], 1);
    assert!(st.admitted[Normal as usize] >= 1); // o CREATE TABLE

    // dentro de uma transação, quem já segura lock entra como "in-txn"
    s.execute("BEGIN").unwrap();
    s.execute("INSERT INTO t VALUES (3)").unwrap(); // ainda sem locks antes dele: usa a prioridade da sessão
    s.execute("INSERT INTO t VALUES (4)").unwrap(); // agora segura o lock da tabela
    s.execute("COMMIT").unwrap();
    assert_eq!(db.scheduler_stats().admitted_in_txn, 1);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM t"), Value::Int(4));
}

#[test]
fn a_statement_blocked_on_a_lock_gives_its_slot_back() {
    // com UMA vaga: o leitor bloqueado pelo escritor ocuparia a única vaga e o escritor
    // nunca conseguiria rodar o próximo comando para terminar (deadlock de admissão)
    let db = open("yield", 1);
    let mut writer = db.session();
    writer.execute("CREATE TABLE t (id INT PRIMARY KEY, v INT); INSERT INTO t VALUES (1, 0)").unwrap();
    writer.execute("BEGIN; UPDATE t SET v = 1").unwrap();

    let (tx, rx) = mpsc::channel();
    let db2 = db.clone();
    let reader = thread::spawn(move || {
        let r = db2.run("SELECT v FROM t").unwrap();
        tx.send(r).unwrap();
    });
    wait_until("leitor bloqueado no lock", || db.lock_snapshot().contains("espera por"));
    assert_eq!(db.scheduler_stats().running, 0, "o leitor bloqueado devolveu a vaga");

    writer.execute("UPDATE t SET v = v + 1").unwrap(); // precisa de vaga: só existe porque o leitor cedeu
    writer.execute("COMMIT").unwrap();

    let result = rx.recv_timeout(Duration::from_secs(5)).expect("o leitor deveria terminar");
    let QueryResult::Rows { rows, .. } = &result[0] else { panic!() };
    assert_eq!(rows, &vec![vec![Value::Int(2)]]);
    reader.join().unwrap();
    assert!(db.scheduler_stats().resumed >= 1);
}

/// Quem bloqueia primeiro espera sozinho; só depois o outro pede o seu lock e fecha o ciclo.
#[derive(Clone, Copy, PartialEq)]
enum First {
    A,
    B,
}

/// Deadlock A↔B (A segura `ta`, B segura `tb`; A quer `tb`, B quer `ta`) com ordem de chegada
/// controlada. Devolve os resultados de A e de B.
fn deadlock(
    db: &Database,
    a_prio: &str,
    b_prio: &str,
    first: First,
    a_extra_lock: bool,
) -> (Result<(), Error>, Result<(), Error>) {
    let mut a = db.session();
    let mut b = db.session();
    a.execute(&format!("SET PRIORITY {a_prio}")).unwrap();
    b.execute(&format!("SET PRIORITY {b_prio}")).unwrap();
    a.execute("BEGIN; UPDATE ta SET v = v + 1").unwrap();
    if a_extra_lock {
        a.execute("UPDATE tc SET v = v + 1").unwrap();
    }
    b.execute("BEGIN; UPDATE tb SET v = v + 1").unwrap();

    let go_a = move || {
        let r = a.execute("UPDATE tb SET v = v + 1").and_then(|_| a.execute("COMMIT")).map(|_| ());
        (r, a)
    };
    let go_b = move || {
        let r = b.execute("UPDATE ta SET v = v + 1").and_then(|_| b.execute("COMMIT")).map(|_| ());
        (r, b)
    };
    let waiting = || db.lock_snapshot().matches("espera por").count();

    let (ta, tb);
    if first == First::A {
        ta = thread::spawn(go_a);
        wait_until("A bloqueado", || waiting() == 1);
        tb = thread::spawn(go_b);
    } else {
        tb = thread::spawn(go_b);
        wait_until("B bloqueado", || waiting() == 1);
        ta = thread::spawn(go_a);
    }
    let (ra, _a) = ta.join().unwrap();
    let (rb, _b) = tb.join().unwrap();
    (ra, rb)
}

fn deadlock_db(name: &str) -> Database {
    let db = open(name, 8);
    db.run(
        "CREATE TABLE ta (id INT PRIMARY KEY, v INT); CREATE TABLE tb (id INT PRIMARY KEY, v INT);
         CREATE TABLE tc (id INT PRIMARY KEY, v INT);
         INSERT INTO ta VALUES (1, 0); INSERT INTO tb VALUES (1, 0); INSERT INTO tc VALUES (1, 0);",
    )
    .unwrap();
    db
}

#[test]
fn deadlock_victim_is_the_lowest_priority_not_the_last_to_ask() {
    // A é HIGH e B é LOW; nas duas ordens de chegada a vítima tem que ser a B
    for (name, first) in [("high_closes", First::B), ("low_closes", First::A)] {
        let db = deadlock_db(&format!("victim_prio_{name}"));
        let (high, low) = deadlock(&db, "HIGH", "LOW", first, false);
        assert!(high.is_ok(), "a sessão HIGH nunca pode ser a vítima ({name}): {high:?}");
        assert!(matches!(low, Err(Error::Deadlock)), "a LOW deve ser a vítima ({name}): {low:?}");
        assert_eq!(count(&db, "SELECT v FROM ta"), Value::Int(1));
        assert_eq!(count(&db, "SELECT v FROM tb"), Value::Int(1)); // só o vencedor aplicou
    }
}

#[test]
fn with_equal_priority_the_victim_is_the_one_with_less_work_to_lose() {
    // A segura 2 locks exclusivos (ta, tc), B só 1 (tb): B perde menos, venha quem vier por último
    for (name, first) in [("a_closes", First::B), ("b_closes", First::A)] {
        let db = deadlock_db(&format!("victim_cost_{name}"));
        let (a, b) = deadlock(&db, "NORMAL", "NORMAL", first, true);
        assert!(a.is_ok(), "A (mais trabalho feito) deveria vencer ({name}): {a:?}");
        assert!(matches!(b, Err(Error::Deadlock)), "B deveria ser a vítima ({name}): {b:?}");
        assert_eq!(count(&db, "SELECT v FROM tc"), Value::Int(1));
    }
}
