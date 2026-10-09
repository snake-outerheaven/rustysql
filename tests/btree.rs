//! B-tree isolada (sem SQL, sem locks lógicos): é aqui que se prova que os latches
//! mantêm a árvore íntegra sob concorrência.

use rustysql::btree::BTree;
use rustysql::bufferpool::BufferPool;
use rustysql::heap::RowId;
use rustysql::keyenc::{prefix_end, value_key};
use rustysql::Value;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

fn pool(name: &str) -> Arc<BufferPool> {
    let p = std::env::temp_dir().join(format!("rustysql_bt_{name}_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&p);
    Arc::new(BufferPool::open(&p, 16384).unwrap())
}

fn ikey(k: i64) -> Vec<u8> {
    value_key(&Value::Int(k))
}

fn rid(k: i64) -> RowId {
    RowId { page: (k / 1000 + 1) as u32, slot: (k % 1000) as u16 }
}

/// Permutação pseudoaleatória determinística (LCG), sem dependências.
fn shuffled(n: i64, seed: u64) -> Vec<i64> {
    let mut v: Vec<i64> = (0..n).collect();
    let mut state = seed;
    for i in (1..v.len()).rev() {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        v.swap(i, (state >> 33) as usize % (i + 1));
    }
    v
}

#[test]
fn random_inserts_search_delete_keep_invariants() {
    let tree = BTree::create(&pool("seq")).unwrap();
    let keys = shuffled(6000, 7);
    for (n, &k) in keys.iter().enumerate() {
        assert!(tree.insert(&ikey(k), rid(k)).unwrap());
        if n % 2000 == 1999 {
            tree.verify().unwrap();
        }
    }
    let stats = tree.verify().unwrap();
    assert_eq!(stats.keys, 6000);
    assert!(stats.height >= 2, "6000 chaves deveriam dividir a raiz: {stats:?}");

    for &k in &keys {
        assert_eq!(tree.search(&ikey(k)).unwrap(), Some(rid(k)));
    }
    assert_eq!(tree.search(&ikey(99999)).unwrap(), None);
    assert!(!tree.insert(&ikey(42), rid(42)).unwrap(), "duplicata deve ser recusada");

    for &k in keys.iter().filter(|k| *k % 2 == 0) {
        assert!(tree.delete(&ikey(k)).unwrap());
    }
    assert!(!tree.delete(&ikey(0)).unwrap());
    let stats = tree.verify().unwrap();
    assert_eq!(stats.keys, 3000);
    for &k in &keys {
        assert_eq!(tree.search(&ikey(k)).unwrap().is_some(), k % 2 == 1, "chave {k}");
    }
}

#[test]
fn range_scan_is_ordered_and_bounded() {
    let tree = BTree::create(&pool("range")).unwrap();
    for &k in &shuffled(3000, 11) {
        tree.insert(&ikey(k * 2), rid(k)).unwrap(); // só chaves pares
    }
    // [1000, 1500): chaves pares de 1000 a 1498
    let got: Vec<RowId> = tree
        .scan(&ikey(1000), Some(&ikey(1500)))
        .unwrap()
        .map(|e| e.unwrap().1)
        .collect();
    let want: Vec<RowId> = (500..750).map(rid).collect();
    assert_eq!(got, want);

    // sem limite superior: vai até o fim, atravessando várias folhas
    assert_eq!(tree.scan(&ikey(0), None).unwrap().count(), 3000);
    // limite inclusivo via prefix_end
    let end = prefix_end(&ikey(1500));
    assert_eq!(tree.scan(&ikey(1000), Some(&end)).unwrap().count(), 251);
    // início entre duas chaves existentes
    let first = tree.scan(&ikey(1001), None).unwrap().next().unwrap().unwrap();
    assert_eq!(first.0, ikey(1002));
}

#[test]
fn long_text_keys_force_deep_tree() {
    let tree = BTree::create(&pool("text")).unwrap();
    let key = |k: i64| value_key(&Value::Text(format!("{k:06}-{}", "x".repeat(300))));
    for &k in &shuffled(1500, 3) {
        tree.insert(&key(k), rid(k)).unwrap();
    }
    let stats = tree.verify().unwrap();
    assert_eq!(stats.keys, 1500);
    assert!(stats.height >= 3, "chaves de ~300 B devem gerar 3 níveis: {stats:?}");
    assert_eq!(tree.search(&key(777)).unwrap(), Some(rid(777)));
    assert!(tree.insert(&value_key(&Value::Text("y".repeat(600))), rid(1)).is_err());
    assert!(tree.insert(&[], rid(1)).is_err());
}

#[test]
fn concurrent_writers_and_readers_preserve_integrity() {
    const WRITERS: i64 = 8;
    const PER_WRITER: i64 = 2500;
    const STRIDE: i64 = WRITERS + 1;
    let tree = BTree::create(&pool("conc")).unwrap();

    // Chaves "âncora" intercaladas com as dos escritores (k % STRIDE == WRITERS): ficam
    // nas mesmas folhas que estão sendo divididas, então as divisões as empurram de
    // página. Um leitor que perca o acoplamento de latches deixa de achá-las.
    let anchor = |i: i64| i * STRIDE + WRITERS;
    for i in 0..PER_WRITER {
        tree.insert(&ikey(anchor(i)), rid(anchor(i))).unwrap();
    }

    let done = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicUsize::new(0));

    let readers: Vec<_> = (0..4)
        .map(|r| {
            let (tree, done, reads) = (tree.clone(), done.clone(), reads.clone());
            thread::spawn(move || {
                let mut i = r;
                while !done.load(Ordering::SeqCst) {
                    let k = anchor(i % PER_WRITER);
                    assert_eq!(tree.search(&ikey(k)).unwrap(), Some(rid(k)), "leitor perdeu a chave {k}");
                    if i % 40 == 0 {
                        // varredura durante as divisões: ordenada, sem repetir e sem perder âncoras
                        let (mut prev, mut anchors): (Option<Vec<u8>>, i64) = (None, 0);
                        for e in tree.scan(&ikey(0), None).unwrap() {
                            let (key, rid) = e.unwrap();
                            assert!(prev.as_ref().is_none_or(|p| *p < key), "varredura fora de ordem");
                            assert_eq!(key, ikey_from_rid(rid), "chave e RowId trocados de lugar");
                            if is_anchor(&key) {
                                anchors += 1;
                            }
                            prev = Some(key);
                        }
                        assert_eq!(anchors, PER_WRITER, "a varredura perdeu chaves âncora");
                    }
                    reads.fetch_add(1, Ordering::SeqCst);
                    i += 7;
                }
            })
        })
        .collect();

    let writers: Vec<_> = (0..WRITERS)
        .map(|w| {
            let tree = tree.clone();
            thread::spawn(move || {
                for &i in &shuffled(PER_WRITER, 100 + w as u64) {
                    let k = i * STRIDE + w;
                    assert!(tree.insert(&ikey(k), rid(k)).unwrap(), "chave {k} inserida duas vezes");
                }
            })
        })
        .collect();

    for w in writers {
        w.join().unwrap();
    }
    done.store(true, Ordering::SeqCst);
    for r in readers {
        r.join().unwrap();
    }
    assert!(reads.load(Ordering::SeqCst) > 0);

    let stats = tree.verify().unwrap();
    assert_eq!(stats.keys, (STRIDE * PER_WRITER) as usize);
    for k in 0..STRIDE * PER_WRITER {
        assert_eq!(tree.search(&ikey(k)).unwrap(), Some(rid(k)), "chave {k} sumiu");
    }

    fn ikey_from_rid(r: RowId) -> Vec<u8> {
        ikey((r.page as i64 - 1) * 1000 + r.slot as i64)
    }
    fn is_anchor(key: &[u8]) -> bool {
        // decodifica a chave Int (tag 0x02 + big-endian com o bit de sinal invertido)
        let n = (u64::from_be_bytes(key[1..9].try_into().unwrap()) ^ (1 << 63)) as i64;
        n % STRIDE == WRITERS
    }
}

#[test]
fn concurrent_inserts_and_deletes_with_deep_tree() {
    const THREADS: i64 = 6;
    const PER_THREAD: i64 = 600;
    let tree = BTree::create(&pool("mixed")).unwrap();
    let key = |k: i64| value_key(&Value::Text(format!("{k:08}-{}", "z".repeat(150))));

    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let tree = tree.clone();
            thread::spawn(move || {
                let mine: Vec<i64> = shuffled(PER_THREAD, 500 + t as u64).iter().map(|i| i * THREADS + t).collect();
                for &k in &mine {
                    assert!(tree.insert(&key(k), rid(k)).unwrap());
                }
                // apaga a metade das próprias chaves enquanto os outros ainda inserem
                for &k in mine.iter().filter(|k| *k % 2 == 0) {
                    assert!(tree.delete(&key(k)).unwrap());
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let stats = tree.verify().unwrap();
    assert_eq!(stats.keys, (THREADS * PER_THREAD / 2) as usize);
    assert!(stats.height >= 3, "{stats:?}");
    for k in 0..THREADS * PER_THREAD {
        assert_eq!(tree.search(&key(k)).unwrap().is_some(), k % 2 == 1, "chave {k}");
    }
}
