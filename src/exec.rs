//! Executor: transforma um `Plan` numa cadeia de iteradores (modelo *Volcano*: cada
//! operador puxa linhas do filho). Operadores que precisam ver tudo antes de emitir
//! (Sort, Aggregate, o lado da construção do hash join) materializam a entrada.

use crate::ast::{AggFunc, Expr, JoinKind};
use crate::btree::BTree;
use crate::catalog::TableInfo;
use crate::error::{Error, Result};
use crate::eval::{bind, BExpr, Scope};
use crate::heap::RowId;
use crate::keyenc::{prefix_end, value_key};
use crate::plan::{AggSpec, Node, Plan};
use crate::value::{decode_row, encode_row, Row, Value};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

/// Fluxo de linhas produzido por um operador. Cada elemento pode ser um erro, que se propaga
/// pela cadeia de operadores.
pub type RowIter = Box<dyn Iterator<Item = Result<Row>>>;
type RidIter = Box<dyn Iterator<Item = Result<(RowId, Row)>>>;

/// Transforma um [`Plan`] numa cadeia de iteradores (modelo *Volcano*: cada operador puxa
/// linhas do filho).
///
/// Operadores que precisam ver tudo antes de emitir (`Sort`, `Aggregate`, o lado de
/// construção do hash join) materializam a entrada.
///
/// # Errors
/// Falha ao ligar as expressões do plano ao escopo, ou ao abrir um scan. Erros durante a
/// iteração chegam como `Err` nos elementos.
pub fn execute(plan: &Plan) -> Result<RowIter> {
    match &plan.node {
        Node::SeqScan { .. } | Node::IndexScan { .. } => {
            Ok(Box::new(scan(plan)?.map(|r| r.map(|(_, row)| row))))
        }
        Node::Filter { input, pred } => {
            let pred = bind(pred, &input.scope)?;
            Ok(Box::new(execute(input)?.filter_map(move |r| match r {
                Err(e) => Some(Err(e)),
                Ok(row) => match pred.test(&row) {
                    Ok(true) => Some(Ok(row)),
                    Ok(false) => None,
                    Err(e) => Some(Err(e)),
                },
            })))
        }
        Node::Project { input, exprs } => {
            let bound: Vec<BExpr> =
                exprs.iter().map(|(e, _)| bind(e, &input.scope)).collect::<Result<_>>()?;
            Ok(Box::new(execute(input)?.map(move |r| {
                let row = r?;
                bound.iter().map(|e| e.eval(&row)).collect()
            })))
        }
        Node::Limit { input, limit, offset } => {
            let it = execute(input)?.skip(*offset);
            Ok(match limit {
                Some(n) => Box::new(it.take(*n)),
                None => Box::new(it),
            })
        }
        Node::Sort { input, keys } => sort(input, keys),
        Node::Aggregate { input, group_by, aggs } => aggregate(input, group_by, aggs),
        Node::NestedLoop { left, right, kind, on } => {
            let combined = left.scope.concat(&right.scope);
            let on = on.as_ref().map(|e| bind(e, &combined)).transpose()?;
            let right_rows: Vec<Row> = execute(right)?.collect::<Result<_>>()?;
            let (kind, width) = (*kind, right.scope.cols.len());
            Ok(Box::new(execute(left)?.flat_map(move |lr| -> Vec<Result<Row>> {
                let l = match lr {
                    Ok(l) => l,
                    Err(e) => return vec![Err(e)],
                };
                let candidates = right_rows.iter().map(|r| r.as_slice());
                join_row(&l, candidates, &on, kind, width)
            })))
        }
        Node::HashJoin { left, right, kind, lkey, rkey, residual } => {
            let combined = left.scope.concat(&right.scope);
            let residual = residual.as_ref().map(|e| bind(e, &combined)).transpose()?;
            let (lkey, rkey) = (bind(lkey, &left.scope)?, bind(rkey, &right.scope)?);
            let mut table: HashMap<Vec<u8>, Vec<Row>> = HashMap::new();
            for r in execute(right)? {
                let row = r?;
                let k = rkey.eval(&row)?;
                if k != Value::Null {
                    table.entry(value_key(&k)).or_default().push(row);
                }
            }
            let (kind, width) = (*kind, right.scope.cols.len());
            Ok(Box::new(execute(left)?.flat_map(move |lr| -> Vec<Result<Row>> {
                let l = match lr {
                    Ok(l) => l,
                    Err(e) => return vec![Err(e)],
                };
                let key = match lkey.eval(&l) {
                    Ok(Value::Null) => None,
                    Ok(v) => Some(value_key(&v)),
                    Err(e) => return vec![Err(e)],
                };
                let empty = Vec::new();
                let bucket = key.and_then(|k| table.get(&k)).unwrap_or(&empty);
                join_row(&l, bucket.iter().map(|r| r.as_slice()), &residual, kind, width)
            })))
        }
        Node::IndexJoin { left, table, alias, index, kind, lkey, residual, right_filter } => {
            let right_scope = Scope::for_table(alias, &table.schema);
            let combined = left.scope.concat(&right_scope);
            let residual = residual.as_ref().map(|e| bind(e, &combined)).transpose()?;
            let right_filter = right_filter.as_ref().map(|e| bind(e, &right_scope)).transpose()?;
            let lkey = bind(lkey, &left.scope)?;
            let (table, tree, unique) = (table.clone(), index.tree.clone(), index.unique);
            let (kind, width) = (*kind, right_scope.cols.len());
            Ok(Box::new(execute(left)?.flat_map(move |lr| -> Vec<Result<Row>> {
                let l = match lr {
                    Ok(l) => l,
                    Err(e) => return vec![Err(e)],
                };
                let matches = lookup_rows(&table, &tree, unique, &lkey, &l, &right_filter);
                match matches {
                    Err(e) => vec![Err(e)],
                    Ok(rows) => join_row(&l, rows.iter().map(|r| r.as_slice()), &residual, kind, width),
                }
            })))
        }
    }
}

/// Linhas da direita que casam com a chave da esquerda, via índice.
fn lookup_rows(
    table: &Arc<TableInfo>,
    tree: &BTree,
    unique: bool,
    lkey: &BExpr,
    left: &Row,
    right_filter: &Option<BExpr>,
) -> Result<Vec<Row>> {
    let v = lkey.eval(left)?;
    if v == Value::Null {
        return Ok(vec![]);
    }
    let key = value_key(&v);
    let mut rids = Vec::new();
    if unique {
        rids.extend(tree.search(&key)?);
    } else {
        for e in tree.scan(&key, Some(&prefix_end(&key)))? {
            rids.push(e?.1);
        }
    }
    let mut rows = Vec::new();
    for rid in rids {
        let bytes = table
            .heap
            .get(rid)?
            .ok_or_else(|| Error::storage("índice aponta para registro inexistente"))?;
        let row = decode_row(&bytes)?;
        if right_filter.as_ref().map_or(Ok(true), |f| f.test(&row))? {
            rows.push(row);
        }
    }
    Ok(rows)
}

/// Combina uma linha da esquerda com os candidatos da direita. Num LEFT JOIN sem
/// nenhum casamento, emite a linha da esquerda estendida com NULLs.
fn join_row<'a>(
    left: &Row,
    candidates: impl Iterator<Item = &'a [Value]>,
    pred: &Option<BExpr>,
    kind: JoinKind,
    right_width: usize,
) -> Vec<Result<Row>> {
    let mut out = Vec::new();
    let mut matched = false;
    for r in candidates {
        let mut row = left.clone();
        row.extend_from_slice(r);
        match pred.as_ref().map_or(Ok(true), |p| p.test(&row)) {
            Ok(true) => {
                matched = true;
                out.push(Ok(row));
            }
            Ok(false) => {}
            Err(e) => {
                out.push(Err(e));
                return out;
            }
        }
    }
    if !matched && kind == JoinKind::Left {
        let mut row = left.clone();
        row.extend(std::iter::repeat_n(Value::Null, right_width));
        out.push(Ok(row));
    }
    out
}

// ---- scans -------------------------------------------------------------

fn scan(plan: &Plan) -> Result<RidIter> {
    match &plan.node {
        Node::SeqScan { table, .. } => Ok(Box::new(
            table.heap.scan().map(|r| r.and_then(|(rid, b)| Ok((rid, decode_row(&b)?)))),
        )),
        Node::IndexScan { table, index, start, end, .. } => {
            let heap = table.heap.clone();
            let it = index.tree.scan(start, end.as_deref())?;
            Ok(Box::new(it.map(move |e| {
                let rid = e?.1;
                let bytes = heap
                    .get(rid)?
                    .ok_or_else(|| Error::storage("índice aponta para registro inexistente"))?;
                Ok((rid, decode_row(&bytes)?))
            })))
        }
        _ => Err(Error::exec("plano de acesso inválido")),
    }
}

/// Linhas (com `RowId`) que satisfazem o plano de UPDATE/DELETE: um scan, opcionalmente
/// sob um Filter. Materializa tudo antes de qualquer modificação (evita reler o que
/// o próprio comando acabou de escrever).
pub fn collect_rids(plan: &Plan) -> Result<Vec<(RowId, Row)>> {
    match &plan.node {
        Node::Filter { input, pred } => {
            let pred = bind(pred, &input.scope)?;
            let mut out = Vec::new();
            for r in scan(input)? {
                let (rid, row) = r?;
                if pred.test(&row)? {
                    out.push((rid, row));
                }
            }
            Ok(out)
        }
        _ => scan(plan)?.collect(),
    }
}

// ---- sort --------------------------------------------------------------

fn sort(input: &Plan, keys: &[(Expr, bool)]) -> Result<RowIter> {
    let bound: Vec<BExpr> = keys.iter().map(|(e, _)| bind(e, &input.scope)).collect::<Result<_>>()?;
    let mut keyed: Vec<(Vec<Value>, Row)> = Vec::new();
    for r in execute(input)? {
        let row = r?;
        let k = bound.iter().map(|e| e.eval(&row)).collect::<Result<Vec<_>>>()?;
        keyed.push((k, row));
    }
    keyed.sort_by(|a, b| {
        for (i, (_, desc)) in keys.iter().enumerate() {
            let o = a.0[i].total_cmp(&b.0[i]);
            let o = if *desc { o.reverse() } else { o };
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    });
    Ok(Box::new(keyed.into_iter().map(|(_, row)| Ok(row))))
}

// ---- agregação ---------------------------------------------------------

enum Acc {
    Count(i64),
    Sum { int: i64, float: f64, saw_float: bool, any: bool },
    Avg { sum: f64, n: i64 },
    Min(Option<Value>),
    Max(Option<Value>),
}

impl Acc {
    fn new(func: AggFunc) -> Acc {
        match func {
            AggFunc::Count => Acc::Count(0),
            AggFunc::Sum => Acc::Sum { int: 0, float: 0.0, saw_float: false, any: false },
            AggFunc::Avg => Acc::Avg { sum: 0.0, n: 0 },
            AggFunc::Min => Acc::Min(None),
            AggFunc::Max => Acc::Max(None),
        }
    }

    /// `v = None` é `COUNT(*)` (conta a linha); NULLs são ignorados nos demais casos.
    fn add(&mut self, v: Option<Value>) -> Result<()> {
        let non_null = match &v {
            None => None,
            Some(Value::Null) => return Ok(()),
            Some(x) => Some(x.clone()),
        };
        match self {
            Acc::Count(n) => *n += 1,
            Acc::Sum { int, float, saw_float, any } => match non_null {
                Some(Value::Int(i)) => {
                    *int = int.checked_add(i).ok_or_else(|| Error::exec("overflow em SUM"))?;
                    *any = true;
                }
                Some(Value::Float(x)) => {
                    *float += x;
                    *saw_float = true;
                    *any = true;
                }
                other => return Err(Error::exec(format!("SUM exige número, recebeu {other:?}"))),
            },
            Acc::Avg { sum, n } => match non_null {
                Some(Value::Int(i)) => {
                    *sum += i as f64;
                    *n += 1;
                }
                Some(Value::Float(x)) => {
                    *sum += x;
                    *n += 1;
                }
                other => return Err(Error::exec(format!("AVG exige número, recebeu {other:?}"))),
            },
            Acc::Min(cur) => {
                let x = non_null.ok_or_else(|| Error::exec("MIN exige um argumento"))?;
                if cur.as_ref().is_none_or(|c| x.total_cmp(c) == Ordering::Less) {
                    *cur = Some(x);
                }
            }
            Acc::Max(cur) => {
                let x = non_null.ok_or_else(|| Error::exec("MAX exige um argumento"))?;
                if cur.as_ref().is_none_or(|c| x.total_cmp(c) == Ordering::Greater) {
                    *cur = Some(x);
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> Value {
        match self {
            Acc::Count(n) => Value::Int(n),
            Acc::Sum { any: false, .. } => Value::Null,
            Acc::Sum { int, float, saw_float: true, .. } => Value::Float(float + int as f64),
            Acc::Sum { int, .. } => Value::Int(int),
            Acc::Avg { n: 0, .. } => Value::Null,
            Acc::Avg { sum, n } => Value::Float(sum / n as f64),
            Acc::Min(v) | Acc::Max(v) => v.unwrap_or(Value::Null),
        }
    }
}

/// Saída: a primeira linha de cada grupo seguida dos valores agregados. Assim
/// expressões sobre as colunas do GROUP BY continuam funcionando depois do agrupamento.
fn aggregate(input: &Plan, group_by: &[Expr], aggs: &[AggSpec]) -> Result<RowIter> {
    let keys: Vec<BExpr> = group_by.iter().map(|e| bind(e, &input.scope)).collect::<Result<_>>()?;
    let args: Vec<Option<BExpr>> = aggs
        .iter()
        .map(|s| s.arg.as_ref().map(|e| bind(e, &input.scope)).transpose())
        .collect::<Result<_>>()?;
    let width = input.scope.cols.len();

    let mut index: HashMap<Vec<u8>, usize> = HashMap::new();
    let mut groups: Vec<(Row, Vec<Acc>)> = Vec::new();
    for r in execute(input)? {
        let row = r?;
        let key = encode_row(&keys.iter().map(|k| k.eval(&row)).collect::<Result<Vec<_>>>()?);
        let g = match index.get(&key) {
            Some(&g) => g,
            None => {
                groups.push((row.clone(), aggs.iter().map(|s| Acc::new(s.func)).collect()));
                index.insert(key, groups.len() - 1);
                groups.len() - 1
            }
        };
        for (acc, arg) in groups[g].1.iter_mut().zip(&args) {
            acc.add(arg.as_ref().map(|a| a.eval(&row)).transpose()?)?;
        }
    }
    if groups.is_empty() && group_by.is_empty() {
        // agregação global sobre zero linhas ainda produz uma linha (COUNT = 0)
        groups.push((vec![Value::Null; width], aggs.iter().map(|s| Acc::new(s.func)).collect()));
    }
    Ok(Box::new(groups.into_iter().map(|(mut row, accs)| {
        row.extend(accs.into_iter().map(Acc::finish));
        Ok(row)
    })))
}
