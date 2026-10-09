//! Gerenciador de prioridade de queries: **controle de admissão**.
//!
//! Latches e locks decidem *quem pode tocar nos dados*. Este módulo decide *quem entra*:
//! no máximo `max_running` comandos executam ao mesmo tempo e os demais esperam numa
//! fila ordenada por prioridade.
//!
//! * **Prioridade**: `HIGH` passa na frente de `NORMAL`, que passa na frente de `LOW`.
//! * **Aging** (anti-inanição): cada comando recebe a chave
//!   `nível × aging_step − instante_de_chegada`. Como o tempo anda igual para todos, a
//!   ordem da fila nunca muda depois de montada (não precisa reordenar), mas quem espera
//!   há mais de `aging_step` por nível de diferença ultrapassa os mais prioritários
//!   recém-chegados. Dentro do mesmo nível a fila é FIFO.
//! * **Quem já segura locks anda mais rápido**: comandos de uma transação explícita
//!   aberta entram antes de qualquer prioridade de usuário (nível `InTransaction`),
//!   porque segurar locks enquanto espera na fila só atrasa todo mundo.
//! * **Ceder a vaga ao bloquear**: um comando esperando um lock de tabela devolve a sua
//!   vaga (`Permit::yield_slot`) e a retoma com prioridade máxima ao ser liberado. Sem
//!   isso, leitores bloqueados por um escritor ocupariam todas as vagas e o próprio
//!   escritor nunca conseguiria entrar para terminar.
//! * COMMIT/ROLLBACK/BEGIN não passam pela fila (liberam recursos, não consomem).

use crate::error::{Error, Result};
use parking_lot::{Condvar, Mutex};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Prioridade de admissão de uma sessão (`SET PRIORITY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Priority {
    /// Em segundo plano: só entra quando não há mais ninguém na frente (ou depois de muito esperar).
    Low = 0,
    /// Padrão.
    #[default]
    Normal = 1,
    /// Passa na frente de `Normal` e `Low`, e é protegida na escolha de vítima de deadlock.
    High = 2,
}

impl Priority {
    /// Interpreta `low`, `normal` ou `high` (sem diferenciar maiúsculas).
    pub fn parse(s: &str) -> Option<Priority> {
        match s.to_ascii_lowercase().as_str() {
            "low" => Some(Priority::Low),
            "normal" => Some(Priority::Normal),
            "high" => Some(Priority::High),
            _ => None,
        }
    }

    /// Nome em maiúsculas (`LOW`, `NORMAL`, `HIGH`).
    pub fn name(self) -> &'static str {
        match self {
            Priority::Low => "LOW",
            Priority::Normal => "NORMAL",
            Priority::High => "HIGH",
        }
    }
}

/// Por que o comando está pedindo vaga: define o nível de partida na fila.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admit {
    /// comando novo, na prioridade da sessão
    New(Priority),
    /// comando dentro de uma transação explícita que já segura locks
    InTransaction,
    /// retomada depois de esperar um lock (a vaga foi cedida enquanto bloqueado)
    Resume,
}

impl Admit {
    fn level(self) -> i128 {
        match self {
            Admit::New(p) => p as i128,
            Admit::InTransaction => 3,
            Admit::Resume => 4,
        }
    }
}

/// Configuração do escalonador de admissão.
#[derive(Debug, Clone)]
pub struct SchedConfig {
    /// comandos executando ao mesmo tempo
    pub max_running: usize,
    /// quanto tempo de espera compensa um nível de prioridade
    pub aging_step: Duration,
    /// quanto um comando aguarda na fila antes de desistir
    pub queue_timeout: Duration,
}

impl Default for SchedConfig {
    fn default() -> Self {
        SchedConfig {
            max_running: 64,
            aging_step: Duration::from_secs(2),
            queue_timeout: Duration::from_secs(30),
        }
    }
}

/// Contadores do escalonador, cumulativos desde que o banco foi aberto (exceto `running`
/// e `queued`, que são o estado atual).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SchedStats {
    /// Comandos executando agora.
    pub running: usize,
    /// Comandos esperando na fila.
    pub queued: usize,
    /// admitidos por prioridade de usuário: [low, normal, high]
    pub admitted: [u64; 3],
    /// Admitidos por já estarem dentro de uma transação que segura locks.
    pub admitted_in_txn: u64,
    /// Retomadas de comandos que tinham cedido a vaga ao bloquear num lock.
    pub resumed: u64,
    /// comandos que precisaram esperar na fila
    pub waited: u64,
    /// Maior tempo que um comando esperou na fila.
    pub max_wait: Duration,
}

#[derive(PartialEq, Eq)]
struct Entry {
    key: i128,
    seq: u64,
}

impl Ord for Entry {
    /// maior chave primeiro; empate: quem chegou antes
    fn cmp(&self, other: &Self) -> Ordering {
        self.key.cmp(&other.key).then(other.seq.cmp(&self.seq))
    }
}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Default)]
struct State {
    running: usize,
    heap: BinaryHeap<Entry>,
    /// desistências (timeout) que ainda estão no heap; descartadas quando chegam ao topo
    cancelled: HashSet<u64>,
    next_seq: u64,
    stats: SchedStats,
}

impl State {
    fn clean_top(&mut self) {
        while let Some(top) = self.heap.peek() {
            if self.cancelled.remove(&top.seq) {
                self.heap.pop();
            } else {
                break;
            }
        }
    }

    fn queued(&self) -> usize {
        self.heap.len() - self.cancelled.len()
    }
}

/// O escalonador de admissão. É `Send + Sync`; use-o por `Arc` (veja [`Scheduler::new`]).
///
/// # Exemplo
/// ```
/// use rustysql::{Admit, Priority, SchedConfig, Scheduler};
///
/// let sched = Scheduler::new(SchedConfig { max_running: 1, ..SchedConfig::default() });
/// let permit = sched.admit(Admit::New(Priority::High))?;   // entra: há uma vaga
/// assert_eq!(sched.stats().running, 1);
/// drop(permit);                                             // a vaga volta
/// assert_eq!(sched.stats().running, 0);
/// # Ok::<(), rustysql::Error>(())
/// ```
pub struct Scheduler {
    state: Mutex<State>,
    wake: Condvar,
    cfg: SchedConfig,
    epoch: Instant,
}

impl Scheduler {
    /// Cria um escalonador (`max_running` é no mínimo 1).
    pub fn new(cfg: SchedConfig) -> Arc<Scheduler> {
        Arc::new(Scheduler {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            cfg: SchedConfig { max_running: cfg.max_running.max(1), ..cfg },
            epoch: Instant::now(),
        })
    }

    /// A configuração em uso.
    pub fn config(&self) -> &SchedConfig {
        &self.cfg
    }

    /// Foto das estatísticas.
    pub fn stats(&self) -> SchedStats {
        let st = self.state.lock();
        SchedStats { running: st.running, queued: st.queued(), ..st.stats }
    }

    /// Espera a vez e devolve uma vaga (`Permit`), liberada quando ele sai de escopo.
    pub fn admit(self: &Arc<Self>, why: Admit) -> Result<Permit> {
        self.acquire(why)?;
        Ok(Permit { sched: self.clone(), held: true, why })
    }

    fn acquire(&self, why: Admit) -> Result<()> {
        let started = Instant::now();
        let deadline = started + self.cfg.queue_timeout;
        let mut st = self.state.lock();
        st.clean_top();

        let seq = st.next_seq;
        st.next_seq += 1;
        let aging = self.cfg.aging_step.as_nanos() as i128;
        let key = why.level() * aging - self.epoch.elapsed().as_nanos() as i128;
        st.heap.push(Entry { key, seq });

        let mut waited = false;
        loop {
            st.clean_top();
            let my_turn = st.heap.peek().is_some_and(|e| e.seq == seq);
            if my_turn && st.running < self.cfg.max_running {
                st.heap.pop();
                st.running += 1;
                match why {
                    Admit::New(p) => st.stats.admitted[p as usize] += 1,
                    Admit::InTransaction => st.stats.admitted_in_txn += 1,
                    Admit::Resume => st.stats.resumed += 1,
                }
                if waited {
                    st.stats.waited += 1;
                    st.stats.max_wait = st.stats.max_wait.max(started.elapsed());
                }
                if st.running < self.cfg.max_running && !st.heap.is_empty() {
                    self.wake.notify_all(); // ainda há vaga para o próximo da fila
                }
                return Ok(());
            }
            waited = true;
            if self.wake.wait_until(&mut st, deadline).timed_out() {
                st.cancelled.insert(seq);
                drop(st);
                self.wake.notify_all(); // quem estava atrás de nós pode ser o próximo
                return Err(Error::QueueTimeout);
            }
        }
    }

    fn release(&self) {
        let mut st = self.state.lock();
        debug_assert!(st.running > 0);
        st.running -= 1;
        drop(st);
        self.wake.notify_all();
    }
}

/// Uma vaga de execução. Devolvida automaticamente no `Drop`.
pub struct Permit {
    sched: Arc<Scheduler>,
    held: bool,
    why: Admit,
}

impl Permit {
    /// Devolve a vaga temporariamente (o comando vai ficar parado esperando um lock).
    pub fn yield_slot(&mut self) {
        if self.held {
            self.held = false;
            self.sched.release();
        }
    }

    /// Pede a vaga de volta, com prioridade máxima: este comando já segura um lock.
    pub fn resume(&mut self) -> Result<()> {
        if !self.held {
            self.sched.acquire(Admit::Resume)?;
            self.held = true;
        }
        Ok(())
    }

    /// O motivo pelo qual esta vaga foi pedida.
    pub fn reason(&self) -> Admit {
        self.why
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if self.held {
            self.sched.release();
        }
    }
}
