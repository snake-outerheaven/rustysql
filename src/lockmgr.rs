//! Gerenciador de locks de tabela (strict two-phase locking).
//!
//! Modos `Shared` (leitores) e `Exclusive` (escritores). Os locks são mantidos até o
//! fim da transação, o que dá isolamento serializável *por tabela*. Quem não consegue
//! o lock espera numa `Condvar`.
//!
//! **Deadlock**: antes de esperar, monta-se o grafo de "quem espera por quem". Se esperar
//! fecha um ciclo, escolhe-se **uma vítima entre os membros do ciclo**, não
//! necessariamente quem chegou por último: a de menor prioridade, depois a que segura
//! menos locks exclusivos (menos trabalho a perder) e, por fim, a mais nova. Se a vítima
//! for outra transação que já está esperando, ela é marcada e acordada.
//! (Sempre abortar quem fecha o ciclo esfomeia os escritores: o leitor, que não fez
//! nada, sempre vence.)
//!
//! **Preferência a escritores**: um leitor novo não fura a fila de quem já espera um lock
//! exclusivo (senão um fluxo contínuo de leitores esfomearia os escritores). Por isso o
//! leitor também passa a "esperar" pelo escritor da fila no grafo de deadlock.

use crate::error::{Error, Result};
use parking_lot::{Condvar, Mutex};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Identificador de uma transação.
pub type TxnId = u64;
/// Identificador de uma tabela (a página raiz do seu heap).
pub type TableId = u32;

/// Prioridade da transação para escolher vítimas (maior = mais protegida).
pub type Rank = u8;

/// Modo de um lock de tabela.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    /// Compartilhado: vários leitores ao mesmo tempo, nenhum escritor.
    Shared,
    /// Exclusivo: um só dono, sem leitores.
    Exclusive,
}

#[derive(Default)]
struct State {
    holders: HashMap<TableId, HashMap<TxnId, LockMode>>,
    /// transação → transações de que ela está esperando um lock
    waiting: HashMap<TxnId, Vec<TxnId>>,
    /// escritores bloqueados, por tabela (têm prioridade sobre leitores novos)
    writers_waiting: HashMap<TableId, HashSet<TxnId>>,
    rank: HashMap<TxnId, Rank>,
    /// vítimas de deadlock que ainda estão dormindo e precisam falhar ao acordar
    victims: HashSet<TxnId>,
}

impl State {
    /// Um ciclo no grafo de espera que passa por `start`, se existir (inclui `start`).
    fn find_cycle(&self, start: TxnId) -> Option<Vec<TxnId>> {
        fn walk(
            st: &State,
            node: TxnId,
            start: TxnId,
            path: &mut Vec<TxnId>,
            seen: &mut HashSet<TxnId>,
        ) -> bool {
            for &next in st.waiting.get(&node).into_iter().flatten() {
                if next == start {
                    return true;
                }
                if seen.insert(next) {
                    path.push(next);
                    if walk(st, next, start, path, seen) {
                        return true;
                    }
                    path.pop();
                }
            }
            false
        }
        let (mut path, mut seen) = (vec![start], HashSet::new());
        walk(self, start, start, &mut path, &mut seen).then_some(path)
    }

    fn exclusive_locks(&self, txn: TxnId) -> usize {
        self.holders.values().filter(|h| h.get(&txn) == Some(&LockMode::Exclusive)).count()
    }

    /// Quem perde menos sendo abortado: menor prioridade, menos locks exclusivos, mais novo.
    fn pick_victim(&self, cycle: &[TxnId]) -> TxnId {
        *cycle
            .iter()
            .min_by_key(|&&t| {
                (self.rank.get(&t).copied().unwrap_or(1), self.exclusive_locks(t), std::cmp::Reverse(t))
            })
            .expect("um ciclo nunca é vazio")
    }
}

/// O gerenciador de locks de tabela. É `Send + Sync`: um único estado atrás de um mutex e uma
/// `Condvar` onde as transações bloqueadas esperam.
pub struct LockManager {
    state: Mutex<State>,
    released: Condvar,
    timeout: Duration,
}

impl LockManager {
    /// Cria um gerenciador; `timeout` é o tempo máximo de espera por um lock.
    pub fn new(timeout: Duration) -> Self {
        LockManager { state: Mutex::new(State::default()), released: Condvar::new(), timeout }
    }

    /// Pega um lock, esperando se preciso. Se a transação já tem um lock `Shared`, pedir
    /// `Exclusive` é um *upgrade*.
    ///
    /// # Errors
    /// [`Error::Deadlock`] se a transação foi escolhida como vítima de um ciclo de espera;
    /// [`Error::LockTimeout`] se a espera passou do limite.
    pub fn acquire(&self, txn: TxnId, table: TableId, mode: LockMode) -> Result<()> {
        self.acquire_with(txn, table, mode, 1, &mut || {})
    }

    /// Como `acquire`, mas informa a prioridade da transação (para escolher vítimas de
    /// deadlock) e chama `on_block` uma vez antes da primeira espera de verdade. O motor
    /// usa isso para devolver a vaga de execução enquanto o comando está parado.
    pub fn acquire_with(
        &self,
        txn: TxnId,
        table: TableId,
        mode: LockMode,
        rank: Rank,
        on_block: &mut dyn FnMut(),
    ) -> Result<()> {
        let mut announced = false;
        let deadline = Instant::now() + self.timeout;
        let mut st = self.state.lock();
        st.rank.insert(txn, rank);
        let outcome = loop {
            if st.victims.remove(&txn) {
                break Err(Error::Deadlock); // outro requisitante nos escolheu como vítima
            }
            let queued: Vec<TxnId> = st
                .writers_waiting
                .get(&table)
                .map(|w| w.iter().copied().filter(|t| *t != txn).collect())
                .unwrap_or_default();
            let holders = st.holders.entry(table).or_default();
            let want = match (holders.get(&txn), mode) {
                (Some(LockMode::Exclusive), _) | (Some(LockMode::Shared), LockMode::Shared) => {
                    break Ok(());
                }
                (_, LockMode::Exclusive) => LockMode::Exclusive, // inclui o upgrade S → X
                (None, LockMode::Shared) => LockMode::Shared,
            };
            let mut blockers: Vec<TxnId> = holders
                .iter()
                .filter(|(t, m)| **t != txn && (want == LockMode::Exclusive || **m == LockMode::Exclusive))
                .map(|(t, _)| *t)
                .collect();
            if want == LockMode::Shared {
                blockers.extend(queued); // não fura a fila dos escritores
            }
            if blockers.is_empty() {
                holders.insert(txn, want);
                break Ok(());
            }
            if want == LockMode::Exclusive {
                st.writers_waiting.entry(table).or_default().insert(txn);
            }
            st.waiting.insert(txn, blockers);

            if let Some(cycle) = st.find_cycle(txn) {
                let victim = st.pick_victim(&cycle);
                if victim == txn {
                    break Err(Error::Deadlock);
                }
                // a vítima é outra transação, que está dormindo neste mesmo gerenciador:
                // marca e acorda; ao acordar ela falha e libera os locks, destravando o ciclo
                st.victims.insert(victim);
                self.released.notify_all();
            }
            if !announced {
                announced = true;
                on_block(); // só toca no escalonador, nunca neste gerenciador: sem inversão de locks
            }
            if self.released.wait_until(&mut st, deadline).timed_out() {
                break Err(Error::LockTimeout);
            }
        };
        st.waiting.remove(&txn);
        if let Some(w) = st.writers_waiting.get_mut(&table) {
            w.remove(&txn);
        }
        drop(st);
        if outcome.is_err() {
            // quem esperava atrás desta transação pode ter ficado livre
            self.released.notify_all();
        }
        outcome
    }

    /// Foto legível do estado dos locks (diagnóstico: quem segura o quê e quem espera por quem).
    pub fn snapshot(&self) -> String {
        let st = self.state.lock();
        let mut out = String::new();
        let mut tables: Vec<_> = st.holders.iter().collect();
        tables.sort_by_key(|(t, _)| **t);
        for (table, holders) in tables {
            let mut h: Vec<_> = holders.iter().map(|(t, m)| format!("txn{t}:{m:?}")).collect();
            h.sort();
            out.push_str(&format!("tabela {table}: {}\n", h.join(", ")));
        }
        let mut waiting: Vec<_> = st.waiting.iter().collect();
        waiting.sort();
        for (t, blockers) in waiting {
            out.push_str(&format!("txn{t} espera por {blockers:?}\n"));
        }
        let mut ww: Vec<_> = st.writers_waiting.iter().filter(|(_, w)| !w.is_empty()).collect();
        ww.sort_by_key(|(t, _)| **t);
        for (table, w) in ww {
            out.push_str(&format!("fila de escritores na tabela {table}: {w:?}\n"));
        }
        out
    }

    /// Solta todos os locks da transação (no commit ou rollback) e acorda quem esperava.
    pub fn release_all(&self, txn: TxnId) {
        let mut st = self.state.lock();
        st.holders.retain(|_, h| {
            h.remove(&txn);
            !h.is_empty()
        });
        st.waiting.remove(&txn);
        st.rank.remove(&txn);
        st.victims.remove(&txn);
        // ninguém mais espera por quem acabou de terminar: sem isso, arestas obsoletas
        // no grafo de espera poderiam fechar ciclos falsos
        st.waiting.values_mut().for_each(|blockers| blockers.retain(|t| *t != txn));
        drop(st);
        self.released.notify_all();
    }
}
