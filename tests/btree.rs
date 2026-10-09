use rustysql::btree;
use rustysql::heap::RowId;
use rustysql::pager::Pager;
use rustysql::{Database, QueryResult, Value};

fn temp(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("rustysql_bt_{name}_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

/// Permutação pseudoaleatória determinística (LCG), sem dependências.
fn shuffled(n: i64) -> Vec<i64> {
    let mut v: Vec<i64> = (0..n).collect();
    let mut state = 0x2545F4914F6CDD1Du64;
    for i in (1..v.len()).rev() {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        v.swap(i, (state >> 33) as usize % (i + 1));
    }
    v
}

fn rid(i: i64) -> RowId {
    RowId { page: (i / 100 + 1) as u32, slot: (i % 100) as u16 }
}

#[test]
fn btree_int_keys_split_search_delete() {
    let mut pager = Pager::open(temp("int")).unwrap();
    let root = btree::create(&mut pager).unwrap();
    let keys = shuffled(5000);
    for &k in &keys {
        btree::insert(&mut pager, root, &Value::Int(k), rid(k)).unwrap();
    }
    let (count, height) = btree::stats(&mut pager, root).unwrap();
    assert_eq!(count, 5000);
    assert!(height >= 2, "5000 chaves deveriam exigir splits (altura {height})");

    for &k in &keys {
        assert_eq!(btree::search(&mut pager, root, &Value::Int(k)).unwrap(), Some(rid(k)));
    }
    assert_eq!(btree::search(&mut pager, root, &Value::Int(9999)).unwrap(), None);
    assert!(btree::insert(&mut pager, root, &Value::Int(42), rid(42)).is_err()); // duplicada

    for &k in keys.iter().filter(|k| *k % 2 == 0) {
        assert!(btree::delete(&mut pager, root, &Value::Int(k)).unwrap());
    }
    assert!(!btree::delete(&mut pager, root, &Value::Int(0)).unwrap());
    for &k in &keys {
        let found = btree::search(&mut pager, root, &Value::Int(k)).unwrap();
        assert_eq!(found.is_some(), k % 2 == 1, "chave {k}");
    }
    assert_eq!(btree::stats(&mut pager, root).unwrap().0, 2500);
}

#[test]
fn btree_text_keys_and_limits() {
    let mut pager = Pager::open(temp("text")).unwrap();
    let root = btree::create(&mut pager).unwrap();
    for &k in &shuffled(1500) {
        let key = Value::Text(format!("chave-longa-para-forcar-splits-{k:06}-{}", "x".repeat(60)));
        btree::insert(&mut pager, root, &key, rid(k)).unwrap();
    }
    let probe = Value::Text(format!("chave-longa-para-forcar-splits-{:06}-{}", 777, "x".repeat(60)));
    assert_eq!(btree::search(&mut pager, root, &probe).unwrap(), Some(rid(777)));
    assert!(btree::insert(&mut pager, root, &Value::Text("y".repeat(600)), rid(1)).is_err());
    assert!(btree::insert(&mut pager, root, &Value::Null, rid(1)).is_err());
}

fn rows(db: &mut Database, sql: &str) -> Vec<Vec<Value>> {
    match db.run(sql).unwrap().pop().unwrap() {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("esperava linhas, veio {other:?}"),
    }
}

#[test]
fn sql_uses_primary_key_index_and_keeps_it_consistent() {
    let path = temp("sql");
    {
        let mut db = Database::open(&path).unwrap();
        db.run("CREATE TABLE t (id INT PRIMARY KEY, v INT)").unwrap();
        let values: Vec<String> = shuffled(2000).iter().map(|k| format!("({k}, {})", k * 10)).collect();
        db.run(&format!("INSERT INTO t VALUES {}", values.join(","))).unwrap();

        assert_eq!(rows(&mut db, "SELECT v FROM t WHERE id = 1234"), vec![vec![Value::Int(12340)]]);
        assert_eq!(rows(&mut db, "SELECT v FROM t WHERE 77 = id AND v > 0"), vec![vec![Value::Int(770)]]);
        assert!(rows(&mut db, "SELECT v FROM t WHERE id = 5000").is_empty());
        assert!(db.run("INSERT INTO t VALUES (1234, 0)").is_err());
        assert!(db.run("INSERT INTO t VALUES (5001, 0), (5001, 1)").is_err()); // duplicada no lote

        // UPDATE mudando a PK: some a chave antiga, aparece a nova
        db.run("UPDATE t SET id = 9000 WHERE id = 10").unwrap();
        assert!(rows(&mut db, "SELECT v FROM t WHERE id = 10").is_empty());
        assert_eq!(rows(&mut db, "SELECT v FROM t WHERE id = 9000"), vec![vec![Value::Int(100)]]);
        assert!(db.run("UPDATE t SET id = 9000 WHERE id = 11").is_err()); // colide

        // trocar chaves em lote (id = id + 10000 desloca todas; sem colisão)
        db.run("UPDATE t SET id = id + 10000 WHERE id < 100").unwrap();
        assert_eq!(rows(&mut db, "SELECT v FROM t WHERE id = 10005"), vec![vec![Value::Int(50)]]);

        db.run("DELETE FROM t WHERE id = 1500").unwrap();
        assert!(rows(&mut db, "SELECT v FROM t WHERE id = 1500").is_empty());
        db.run("INSERT INTO t VALUES (1500, 1)").unwrap(); // chave liberada
    }
    // reabre: o índice veio do disco
    let mut db = Database::open(&path).unwrap();
    assert_eq!(rows(&mut db, "SELECT v FROM t WHERE id = 1500"), vec![vec![Value::Int(1)]]);
    assert_eq!(rows(&mut db, "SELECT v FROM t WHERE id = 1999"), vec![vec![Value::Int(19990)]]);
    assert_eq!(rows(&mut db, "SELECT id FROM t").len(), 2000 - 1 /*10 mudou, não saiu*/ + 1);
}
