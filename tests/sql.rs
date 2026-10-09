use rustysql::{Database, Options, QueryResult, Value};
use std::path::PathBuf;

fn temp_db(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("rustysql_sql_{name}_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_file(format!("{}.wal", p.display()));
    p
}

fn open(name: &str) -> Database {
    // sem fsync: estes testes não são sobre durabilidade
    let opts = Options { sync_commits: false, ..Options::default() };
    Database::open_with(temp_db(name), opts).unwrap()
}

fn rows(db: &Database, sql: &str) -> Vec<Vec<Value>> {
    match db.run(sql).unwrap_or_else(|e| panic!("{sql}: {e}")).pop().unwrap() {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("esperava linhas, veio {other:?}"),
    }
}

fn explain(db: &Database, sql: &str) -> String {
    match db.run(&format!("EXPLAIN {sql}")).unwrap().pop().unwrap() {
        QueryResult::Done(text) => text,
        other => panic!("{other:?}"),
    }
}

fn i(n: i64) -> Value {
    Value::Int(n)
}
fn t(s: &str) -> Value {
    Value::Text(s.into())
}
fn f(x: f64) -> Value {
    Value::Float(x)
}

fn company(db: &Database) {
    db.run(
        "CREATE TABLE dept (id INT PRIMARY KEY, name TEXT);
         CREATE TABLE emp (id INT PRIMARY KEY, name TEXT, dept_id INT, salary FLOAT);
         INSERT INTO dept VALUES (1,'eng'),(2,'ops'),(3,'hr');
         INSERT INTO emp VALUES (1,'ana',1,100),(2,'bia',1,200),(3,'caio',2,150),(4,'davi',NULL,50),(5,'eva',9,70);",
    )
    .unwrap();
}

#[test]
fn secondary_index_is_chosen_and_stays_consistent() {
    let db = open("secondary");
    db.run("CREATE TABLE big (id INT PRIMARY KEY, grp INT, txt TEXT)").unwrap();
    let values: Vec<String> = (0..1000).map(|k| format!("({k}, {}, 'linha {k}')", k % 10)).collect();
    db.run(&format!("INSERT INTO big VALUES {}", values.join(","))).unwrap();

    let before = rows(&db, "SELECT id FROM big WHERE grp = 3");
    assert_eq!(before.len(), 100);
    assert!(explain(&db, "SELECT id FROM big WHERE grp = 3").contains("SeqScan"));

    db.run("CREATE INDEX idx_grp ON big(grp)").unwrap(); // carrega a partir da tabela existente
    let plan = explain(&db, "SELECT id FROM big WHERE grp = 3");
    assert!(plan.contains("IndexScan big using idx_grp (grp = 3)"), "{plan}");
    assert_eq!(rows(&db, "SELECT id FROM big WHERE grp = 3"), before); // mesmo resultado

    // intervalos: um lado, dois lados, exclusivo/inclusivo
    assert_eq!(rows(&db, "SELECT id FROM big WHERE grp > 7").len(), 200);
    assert_eq!(rows(&db, "SELECT id FROM big WHERE grp >= 2 AND grp < 4").len(), 200);
    assert_eq!(rows(&db, "SELECT id FROM big WHERE grp BETWEEN 2 AND 4").len(), 300);
    let plan = explain(&db, "SELECT id FROM big WHERE grp >= 2 AND grp < 4");
    assert!(plan.contains("grp >= 2 AND grp < 4"), "{plan}");
    assert_eq!(rows(&db, "SELECT id FROM big WHERE 5 > grp AND grp > 3").len(), 100); // literal à esquerda

    // o índice acompanha UPDATE e DELETE
    db.run("UPDATE big SET grp = 99 WHERE id = 3").unwrap();
    assert_eq!(rows(&db, "SELECT id FROM big WHERE grp = 3").len(), 99);
    assert_eq!(rows(&db, "SELECT id FROM big WHERE grp = 99"), vec![vec![i(3)]]);
    db.run("DELETE FROM big WHERE grp = 5").unwrap();
    assert!(rows(&db, "SELECT id FROM big WHERE grp = 5").is_empty());
    assert_eq!(rows(&db, "SELECT id FROM big").len(), 900);

    db.run("DROP INDEX idx_grp").unwrap();
    assert!(explain(&db, "SELECT id FROM big WHERE grp = 3").contains("SeqScan"));
    assert_eq!(rows(&db, "SELECT id FROM big WHERE grp = 3").len(), 99);
    assert!(db.run("DROP INDEX pk_big").is_err(), "o índice da PK não pode sair");
}

#[test]
fn primary_key_uses_the_index_even_inside_and() {
    let db = open("pkplan");
    company(&db);
    let plan = explain(&db, "SELECT * FROM emp WHERE id = 3");
    assert!(plan.contains("IndexScan emp using pk_emp (id = 3)"), "{plan}");
    let plan = explain(&db, "SELECT * FROM emp WHERE salary > 60 AND id = 3");
    assert!(plan.contains("IndexScan") && plan.contains("Filter"), "o filtro completo é reaplicado: {plan}");
    assert_eq!(rows(&db, "SELECT name FROM emp WHERE salary > 60 AND id = 3"), vec![vec![t("caio")]]);
    assert!(rows(&db, "SELECT name FROM emp WHERE salary > 200 AND id = 3").is_empty());
    // UPDATE e DELETE também planejam pelo índice
    assert!(explain(&db, "UPDATE emp SET salary = 1 WHERE id = 2").contains("IndexScan"));
    assert!(explain(&db, "DELETE FROM emp WHERE id > 3").contains("IndexScan"));
    assert!(explain(&db, "SELECT * FROM emp WHERE name = 'ana'").contains("SeqScan"));
}

#[test]
fn unique_index_and_statement_atomicity() {
    let db = open("unique");
    db.run(
        "CREATE TABLE u (id INT PRIMARY KEY, email TEXT);
         CREATE UNIQUE INDEX u_email ON u(email);
         INSERT INTO u VALUES (1,'a'),(2,NULL),(3,NULL);",
    )
    .unwrap(); // vários NULL não violam a unicidade
    assert!(db.run("INSERT INTO u VALUES (4,'a')").is_err());
    // um comando que falha desfaz tudo o que ele já tinha feito
    assert!(db.run("INSERT INTO u VALUES (10,'x'),(11,'a')").is_err());
    assert!(rows(&db, "SELECT id FROM u WHERE id = 10").is_empty());
    assert!(rows(&db, "SELECT id FROM u WHERE email = 'x'").is_empty());
    assert!(db.run("UPDATE u SET email = 'a' WHERE id = 3").is_err());
    assert_eq!(rows(&db, "SELECT email FROM u WHERE id = 3"), vec![vec![Value::Null]]);
    assert_eq!(rows(&db, "SELECT id FROM u").len(), 3);

    db.run("CREATE TABLE d (id INT PRIMARY KEY, v INT); INSERT INTO d VALUES (1,5),(2,5)").unwrap();
    assert!(db.run("CREATE UNIQUE INDEX d_v ON d(v)").is_err());
    assert!(db.describe("d").unwrap().indexes.iter().all(|(n, _, _)| n != "d_v"), "índice falho não pode sobrar");
    db.run("CREATE INDEX d_v ON d(v)").unwrap(); // o nome ficou livre
}

#[test]
fn joins_choose_the_right_algorithm_and_compute_correctly() {
    let db = open("joins");
    company(&db);

    // dept.id é PK: index nested loop
    let q = "SELECT e.name, d.name FROM emp e JOIN dept d ON e.dept_id = d.id ORDER BY e.id";
    assert!(explain(&db, q).contains("IndexNestedLoopJoin Inner probe dept AS d using pk_dept"), "{}", explain(&db, q));
    assert_eq!(
        rows(&db, q),
        vec![vec![t("ana"), t("eng")], vec![t("bia"), t("eng")], vec![t("caio"), t("ops")]]
    );

    // LEFT JOIN preenche com NULL quem não casa (dept_id NULL e dept_id sem dept)
    let q = "SELECT e.id, d.name FROM emp e LEFT JOIN dept d ON e.dept_id = d.id ORDER BY e.id";
    assert_eq!(
        rows(&db, q),
        vec![
            vec![i(1), t("eng")],
            vec![i(2), t("eng")],
            vec![i(3), t("ops")],
            vec![i(4), Value::Null],
            vec![i(5), Value::Null],
        ]
    );

    // sem índice na coluna: hash join (NULL nunca casa)
    let q = "SELECT a.id, b.id FROM emp a JOIN emp b ON a.dept_id = b.dept_id";
    assert!(explain(&db, q).contains("HashJoin"), "{}", explain(&db, q));
    assert_eq!(rows(&db, q).len(), 6); // (1,1) (1,2) (2,1) (2,2) (3,3) (5,5); o NULL de davi não casa

    // desigualdade: nested loop
    let q = "SELECT a.id FROM emp a JOIN emp b ON a.salary < b.salary";
    assert!(explain(&db, q).contains("NestedLoopJoin"), "{}", explain(&db, q));
    assert_eq!(rows(&db, q).len(), 10);

    // um índice secundário na coluna de junção troca o hash join por index nested loop
    db.run("CREATE INDEX emp_dept ON emp(dept_id)").unwrap();
    let q = "SELECT d.name, e.name FROM dept d JOIN emp e ON e.dept_id = d.id ORDER BY e.id";
    assert!(explain(&db, q).contains("IndexNestedLoopJoin Inner probe emp AS e using emp_dept"), "{}", explain(&db, q));
    assert_eq!(rows(&db, q).len(), 3);

    // predicados do WHERE e do ON descem para o scan da tabela certa
    let q = "SELECT e.name FROM emp e JOIN dept d ON e.dept_id = d.id WHERE d.id = 1 AND e.salary > 150";
    assert_eq!(rows(&db, q), vec![vec![t("bia")]]);
    let plan = explain(&db, q);
    assert!(plan.contains("IndexScan dept AS d using pk_dept (id = 1)") || plan.contains("filter"), "{plan}");

    // coluna ambígua e alias repetido são erros claros
    assert!(db.run("SELECT name FROM emp e JOIN dept d ON e.dept_id = d.id").is_err());
    assert!(db.run("SELECT * FROM emp e JOIN dept e ON e.dept_id = e.id").is_err());
}

#[test]
fn aggregates_group_by_having_order_limit() {
    let db = open("aggs");
    company(&db);

    assert_eq!(
        rows(&db, "SELECT COUNT(*), COUNT(dept_id), SUM(salary), AVG(salary), MIN(salary), MAX(salary) FROM emp"),
        vec![vec![i(5), i(4), f(570.0), f(114.0), f(50.0), f(200.0)]]
    );
    // agregação global sobre zero linhas ainda devolve uma linha
    assert_eq!(rows(&db, "SELECT COUNT(*), SUM(salary) FROM emp WHERE id > 100"), vec![vec![i(0), Value::Null]]);

    assert_eq!(
        rows(&db, "SELECT dept_id, COUNT(*) AS n FROM emp GROUP BY dept_id HAVING COUNT(*) > 1 ORDER BY n DESC"),
        vec![vec![i(1), i(2)]]
    );
    // GROUP BY depois de um LEFT JOIN; COUNT(coluna) ignora o NULL do lado sem par
    assert_eq!(
        rows(
            &db,
            "SELECT d.name, COUNT(e.id), SUM(e.salary) FROM dept d LEFT JOIN emp e ON e.dept_id = d.id \
             GROUP BY d.name ORDER BY d.name"
        ),
        vec![
            vec![t("eng"), i(2), f(300.0)],
            vec![t("hr"), i(0), Value::Null],
            vec![t("ops"), i(1), f(150.0)],
        ]
    );
    // expressão sobre agregados e ORDER BY por alias, posição e LIMIT/OFFSET
    assert_eq!(
        rows(&db, "SELECT dept_id, MAX(salary) - MIN(salary) AS spread FROM emp WHERE dept_id IS NOT NULL GROUP BY dept_id ORDER BY 2 DESC"),
        vec![vec![i(1), f(100.0)], vec![i(2), f(0.0)], vec![i(9), f(0.0)]]
    );
    assert_eq!(
        rows(&db, "SELECT name, salary * 2 AS dbl FROM emp ORDER BY dbl DESC LIMIT 2 OFFSET 1"),
        vec![vec![t("caio"), f(300.0)], vec![t("ana"), f(200.0)]]
    );
    assert!(db.run("SELECT * FROM emp WHERE COUNT(*) > 1").is_err());
}

#[test]
fn expressions_like_between_in() {
    let db = open("expr");
    company(&db);
    let names = |sql: &str| -> Vec<String> {
        rows(&db, sql).into_iter().map(|r| r[0].to_string()).collect()
    };
    assert_eq!(names("SELECT name FROM emp WHERE name LIKE 'b%'"), ["bia"]);
    assert_eq!(names("SELECT name FROM emp WHERE name LIKE '_i%'"), ["bia"]);
    assert_eq!(names("SELECT name FROM emp WHERE name NOT LIKE '%a%' ORDER BY id"), Vec::<String>::new());
    assert_eq!(names("SELECT name FROM emp WHERE salary BETWEEN 70 AND 150 ORDER BY id"), ["ana", "caio", "eva"]);
    assert_eq!(names("SELECT name FROM emp WHERE salary NOT BETWEEN 70 AND 150 ORDER BY id"), ["bia", "davi"]);
    assert_eq!(names("SELECT name FROM emp WHERE id IN (1, 3, 5) ORDER BY id"), ["ana", "caio", "eva"]);
    assert_eq!(names("SELECT name FROM emp WHERE id NOT IN (1, 3, 5) ORDER BY id"), ["bia", "davi"]);
    assert_eq!(names("SELECT name FROM emp WHERE dept_id IS NULL"), ["davi"]);
    // lógica de três valores: NULL nunca é verdadeiro
    assert_eq!(names("SELECT name FROM emp WHERE dept_id <> 1 ORDER BY id"), ["caio", "eva"]);
    assert_eq!(names("SELECT e.name FROM emp AS e WHERE e.id = 2"), ["bia"]);
    assert_eq!(rows(&db, "SELECT -salary + 1 FROM emp WHERE id = 1"), vec![vec![f(-99.0)]]);
    assert_eq!(names("SELECT name FROM emp WHERE id % 2 = 0 ORDER BY id"), ["bia", "davi"]);
    assert_eq!(rows(&db, "SELECT id % 3, salary % 60 FROM emp WHERE id = 2"), vec![vec![i(2), f(20.0)]]);
}

#[test]
fn update_changing_primary_keys_in_bulk_and_ddl_errors() {
    let db = open("pkshift");
    db.run("CREATE TABLE t (id INT PRIMARY KEY, v INT)").unwrap();
    let values: Vec<String> = (0..300).map(|k| format!("({k}, {})", k * 10)).collect();
    db.run(&format!("INSERT INTO t VALUES {}", values.join(","))).unwrap();

    db.run("UPDATE t SET id = id + 1000").unwrap(); // desloca todas as chaves, sem falsas colisões
    assert!(rows(&db, "SELECT v FROM t WHERE id = 5").is_empty());
    assert_eq!(rows(&db, "SELECT v FROM t WHERE id = 1005"), vec![vec![i(50)]]);
    assert_eq!(rows(&db, "SELECT id FROM t").len(), 300);
    assert!(db.run("UPDATE t SET id = 1000 WHERE id = 1001").is_err()); // colide de verdade
    assert_eq!(rows(&db, "SELECT v FROM t WHERE id = 1001"), vec![vec![i(10)]]);

    assert!(db.run("CREATE TABLE t (x INT)").is_err());
    assert!(db.run("CREATE TABLE z (a INT, a INT)").is_err());
    assert!(db.run("CREATE INDEX i1 ON t(nope)").is_err());
    assert!(db.run("DROP TABLE nope").is_err());
    db.run("DROP TABLE t").unwrap();
    assert!(db.run("SELECT * FROM t").is_err());
    db.run("CREATE TABLE t (x INT)").unwrap(); // o nome volta a ficar livre
}
