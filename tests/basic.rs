use rustysql::{Database, QueryResult, Value};
use std::path::PathBuf;

fn temp_db(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("rustysql_test_{name}_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn rows(db: &mut Database, sql: &str) -> Vec<Vec<Value>> {
    match db.run(sql).unwrap().pop().unwrap() {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("esperava linhas, veio {other:?}"),
    }
}

#[test]
fn crud_flow() {
    let path = temp_db("crud");
    let mut db = Database::open(&path).unwrap();
    db.run("CREATE TABLE users (id INT PRIMARY KEY, name TEXT NOT NULL, score FLOAT)").unwrap();
    db.run("INSERT INTO users VALUES (1,'ana',9.5),(2,'bia',7),(3,'caio',NULL)").unwrap();

    let r = rows(&mut db, "SELECT name FROM users WHERE score > 8");
    assert_eq!(r, vec![vec![Value::Text("ana".into())]]);

    let r = rows(&mut db, "SELECT id FROM users ORDER BY id DESC LIMIT 2");
    assert_eq!(r, vec![vec![Value::Int(3)], vec![Value::Int(2)]]);

    // NULL nunca satisfaz comparações; IS NULL sim
    assert_eq!(rows(&mut db, "SELECT id FROM users WHERE score < 100").len(), 2);
    assert_eq!(rows(&mut db, "SELECT id FROM users WHERE score IS NULL").len(), 1);

    db.run("UPDATE users SET score = score + 1 WHERE id = 2").unwrap();
    assert_eq!(rows(&mut db, "SELECT score FROM users WHERE id = 2"), vec![vec![Value::Float(8.0)]]);

    db.run("DELETE FROM users WHERE id = 1").unwrap();
    assert_eq!(rows(&mut db, "SELECT * FROM users").len(), 2);
}

#[test]
fn constraints() {
    let path = temp_db("constraints");
    let mut db = Database::open(&path).unwrap();
    db.run("CREATE TABLE t (id INT PRIMARY KEY, name TEXT NOT NULL)").unwrap();
    db.run("INSERT INTO t VALUES (1,'a')").unwrap();
    assert!(db.run("INSERT INTO t VALUES (1,'b')").is_err()); // PK duplicada
    assert!(db.run("INSERT INTO t VALUES (2,NULL)").is_err()); // NOT NULL
    assert!(db.run("INSERT INTO t VALUES ('x','b')").is_err()); // tipo errado
    assert!(db.run("SELECT * FROM nada").is_err());
    assert_eq!(rows(&mut db, "SELECT * FROM t").len(), 1);
}

#[test]
fn persistence_and_many_pages() {
    let path = temp_db("persist");
    {
        let mut db = Database::open(&path).unwrap();
        db.run("CREATE TABLE big (id INT PRIMARY KEY, txt TEXT)").unwrap();
        for i in 0..500 {
            db.run(&format!("INSERT INTO big VALUES ({i}, 'linha numero {i} com algum texto')")).unwrap();
        }
    }
    let mut db = Database::open(&path).unwrap(); // reabre do disco
    assert_eq!(db.table_names(), vec!["big".to_string()]);
    assert_eq!(rows(&mut db, "SELECT id FROM big").len(), 500);
    assert_eq!(
        rows(&mut db, "SELECT txt FROM big WHERE id = 499"),
        vec![vec![Value::Text("linha numero 499 com algum texto".into())]]
    );
    db.run("DELETE FROM big WHERE id < 250").unwrap();
    db.run("INSERT INTO big VALUES (1000, 'reaproveita slots')").unwrap();
    assert_eq!(rows(&mut db, "SELECT id FROM big").len(), 251);
}
