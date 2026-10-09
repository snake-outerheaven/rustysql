"use strict";
const { invoke } = window.__TAURI__.core;
const $ = (id) => document.getElementById(id);

let opened = false;
let tables = [];
let page = 0;
const PAGE = 100;
let monitorTimer = null;

// ---- utilidades -----------------------------------------------------------

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function toast(msg) {
  const t = $("toast");
  t.textContent = msg;
  t.hidden = false;
  clearTimeout(toast.h);
  toast.h = setTimeout(() => (t.hidden = true), 4000);
}

const fmtMs = (ms) => (ms < 1 ? ms.toFixed(2) : ms < 100 ? ms.toFixed(1) : Math.round(ms)) + " ms";
const fmtBytes = (n) => (n < 1024 ? n + " B" : n < 1048576 ? (n / 1024).toFixed(1) + " KiB" : (n / 1048576).toFixed(2) + " MiB");

function renderTable(columns, rows) {
  const wrap = el("div", "wrap");
  const t = el("table", "t");
  const head = el("tr");
  columns.forEach((c) => head.append(el("th", "", c)));
  const thead = el("thead");
  thead.append(head);
  t.append(thead);
  const body = el("tbody");
  rows.forEach((r) => {
    const tr = el("tr");
    r.forEach((v) => {
      let td;
      if (v === null) td = el("td", "null", "NULL");
      else if (typeof v === "number") td = el("td", "num", String(v));
      else if (typeof v === "boolean") td = el("td", "bool", String(v));
      else td = el("td", "", v);
      tr.append(td);
    });
    body.append(tr);
  });
  t.append(body);
  wrap.append(t);
  return wrap;
}

// ---- banco e esquema ------------------------------------------------------

function setOpen(on) {
  opened = on;
  $("btn-open").disabled = false;
  $("btn-close").disabled = !on;
  $("tables-sec").hidden = !on;
  if (!on) {
    $("status").textContent = "nenhum banco aberto";
    $("tables").replaceChildren();
    tables = [];
  }
}

function showSchema(info) {
  tables = info.tables;
  $("status").textContent = "aberto: " + info.path;
  const ul = $("tables");
  ul.replaceChildren();
  tables.forEach((t) => {
    const li = el("li");
    const head = el("div", "t");
    head.append(el("span", "", t.name), el("span", "n", (t.rows ?? "?") + " linhas"));
    const cols = el("div", "cols");
    t.columns.forEach((c) => {
      const line = el("div");
      line.append(el("span", c.pk ? "pk" : "", c.name), document.createTextNode(" " + c.type + (c.not_null && !c.pk ? " NOT NULL" : "")));
      if (c.pk) line.append(el("span", "pk", " ◆ PK"));
      cols.append(line);
    });
    t.indexes.forEach((i) => cols.append(el("div", "ix", `⌥ ${i.name} (${i.column})${i.unique ? " único" : ""}`)));
    li.append(head, cols);
    li.onclick = (ev) => {
      if (ev.detail === 2) { openData(t.name); return; }
      li.classList.toggle("open");
    };
    li.title = "clique: colunas · duplo clique: ver dados";
    ul.append(li);
  });
  const sel = $("data-table");
  const keep = sel.value;
  sel.replaceChildren(...tables.map((t) => new Option(t.name, t.name)));
  if (tables.some((t) => t.name === keep)) sel.value = keep;
}

async function refreshSchema() {
  if (!opened) return;
  try { showSchema(await invoke("schema")); } catch (e) { toast(String(e)); }
}

$("btn-open").onclick = async () => {
  $("btn-open").disabled = true;
  try {
    showSchema(await invoke("open_db", { path: $("path").value }));
    setOpen(true);
    $("txn").hidden = true;
    toast("banco aberto");
  } catch (e) { toast(String(e)); }
  $("btn-open").disabled = false;
};
$("btn-close").onclick = async () => {
  await invoke("close_db");
  setOpen(false);
  $("out").replaceChildren();
  $("txn").hidden = true;
};
$("btn-refresh").onclick = refreshSchema;
$("btn-ckpt").onclick = async () => {
  try { toast("checkpoint em " + fmtMs(await invoke("checkpoint"))); } catch (e) { toast(String(e)); }
};
$("btn-demo").onclick = async () => {
  $("btn-demo").disabled = true;
  try {
    const info = await invoke("load_demo");
    showSchema(info);
    toast("demo carregada em " + fmtMs(info.ms));
  } catch (e) { toast(String(e)); }
  $("btn-demo").disabled = false;
};

// ---- abas -----------------------------------------------------------------

function selectTab(name) {
  document.querySelectorAll("#tabs button").forEach((b) => b.classList.toggle("active", b.dataset.tab === name));
  document.querySelectorAll(".tab").forEach((t) => t.classList.toggle("active", t.id === "tab-" + name));
  clearInterval(monitorTimer);
  if (name === "monitor") { pollMonitor(); monitorTimer = setInterval(pollMonitor, 1000); }
  if (name === "data" && opened) loadData();
}
document.querySelectorAll("#tabs button").forEach((b) => (b.onclick = () => selectTab(b.dataset.tab)));

// ---- console SQL ----------------------------------------------------------

function showRun(r) {
  const out = $("out");
  out.replaceChildren();
  $("txn").hidden = !r.in_txn;
  $("timing").textContent = fmtMs(r.ms);
  if (!r.ok) {
    out.append(el("div", "msg " + (r.kind === "transient" ? "transient" : "err"), r.error));
    return;
  }
  r.results.forEach((res, i) => {
    const box = el("div", "result");
    if (res.kind === "rows") {
      box.append(el("div", "cap", `${res.rows.length} linha(s)`), renderTable(res.columns, res.rows));
    } else if (res.kind === "affected") {
      box.append(el("div", "msg", `${res.n} linha(s) afetada(s)`));
    } else {
      box.append(el("div", "msg", res.message));
    }
    out.append(box);
  });
  if (!r.results.length) out.append(el("div", "msg", "ok"));
}

async function runSql() {
  if (!opened) return toast("abra um banco primeiro");
  const sql = $("sql").value.trim();
  if (!sql) return;
  $("btn-run").disabled = true;
  try {
    const r = await invoke("run_sql", { sql, priority: $("prio").value });
    showRun(r);
    if (/\b(create|drop|insert|delete)\b/i.test(sql)) refreshSchema();
  } catch (e) { toast(String(e)); }
  $("btn-run").disabled = false;
}
$("btn-run").onclick = runSql;
$("sql").addEventListener("keydown", (e) => {
  if (e.ctrlKey && e.key === "Enter") { e.preventDefault(); runSql(); }
  if (e.key === "Tab") {
    e.preventDefault();
    const t = e.target, s = t.selectionStart;
    t.setRangeText("  ", s, t.selectionEnd, "end");
  }
});
$("btn-explain").onclick = async () => {
  if (!opened) return toast("abra um banco primeiro");
  const sql = $("sql").value.trim();
  if (!sql) return;
  const out = $("out");
  out.replaceChildren();
  try {
    out.append(el("div", "msg", await invoke("explain", { sql })));
  } catch (e) { out.append(el("div", "msg err", String(e))); }
};

// ---- dados ----------------------------------------------------------------

function openData(name) {
  $("data-table").value = name;
  page = 0;
  selectTab("data");
}

async function loadData() {
  const name = $("data-table").value;
  if (!name) { $("grid").replaceChildren(el("p", "muted", "nenhuma tabela")); return; }
  try {
    const r = await invoke("browse", { table: name, limit: PAGE, offset: page * PAGE });
    $("grid").replaceChildren(renderTable(r.columns, r.rows));
    const total = tables.find((t) => t.name === name)?.rows ?? "?";
    $("pg-info").textContent = `linhas ${page * PAGE + 1}–${page * PAGE + r.rows.length} de ${total}`;
    $("pg-prev").disabled = page === 0;
    $("pg-next").disabled = r.rows.length < PAGE;
  } catch (e) { toast(String(e)); }
}
$("data-table").onchange = () => { page = 0; loadData(); };
$("pg-prev").onclick = () => { page = Math.max(0, page - 1); loadData(); };
$("pg-next").onclick = () => { page++; loadData(); };

// ---- monitor --------------------------------------------------------------

function card(k, v, s) {
  const c = el("div", "card");
  c.append(el("div", "k", k), el("div", "v", String(v)));
  if (s) c.append(el("div", "s", s));
  return c;
}

async function pollMonitor() {
  if (!opened) { $("cards").replaceChildren(el("p", "muted", "abra um banco")); return; }
  try {
    const m = await invoke("monitor");
    const s = m.sched;
    $("cards").replaceChildren(
      card("páginas sujas", m.dirty_pages, "no buffer pool"),
      card("tamanho do WAL", fmtBytes(m.wal_bytes), "0 após checkpoint"),
      card("executando", s.running, "comandos admitidos"),
      card("na fila", s.queued, "esperando vaga"),
      card("admitidos", `${s.admitted[0]} / ${s.admitted[1]} / ${s.admitted[2]}`, "baixa / normal / alta"),
      card("em transação", s.admitted_in_txn, "passam direto"),
      card("retomadas", s.resumed, "voltaram após lock"),
      card("esperaram na fila", s.waited, "maior espera " + fmtMs(s.max_wait_ms)),
    );
    $("locks").textContent = m.locks && m.locks.trim() ? m.locks : "(nenhum lock ativo)";
  } catch (e) { /* banco pode ter sido fechado entre dois ciclos */ }
}

// ---- laboratório ----------------------------------------------------------

$("btn-lab").onclick = async () => {
  if (!opened) return toast("abra um banco primeiro");
  const btn = $("btn-lab");
  btn.disabled = true;
  btn.textContent = "rodando…";
  const out = $("lab-out");
  out.replaceChildren();
  try {
    const r = await invoke("run_lab", { workers: +$("lab-w").value, ops: +$("lab-n").value });
    const max = Math.max(...r.workers.map((w) => w.ms));
    const bars = el("div", "bars");
    r.workers.forEach((w) => {
      const row = el("div", "wk");
      const track = el("div", "track");
      const fill = el("div", "fill " + w.priority);
      fill.style.width = (w.ms / max) * 100 + "%";
      track.append(fill);
      row.append(
        el("div", "", `#${w.id} ${w.priority}`),
        track,
        el("div", "info", `${fmtMs(w.ms)} · ${w.transfers} ok · ${w.retries} repetições`),
      );
      bars.append(row);
    });
    const v = el("div", "verdict" + (r.consistent ? "" : " bad"));
    v.textContent = r.consistent
      ? `Íntegro: soma dos saldos ${r.sum_before} → ${r.sum_after}. Total ${fmtMs(r.total_ms)}.`
      : `INCONSISTENTE: soma ${r.sum_before} → ${r.sum_after}`;
    out.append(bars, v);
    refreshSchema();
  } catch (e) { out.append(el("div", "msg err", String(e))); }
  btn.disabled = false;
  btn.textContent = "Rodar";
};

setOpen(false);
