//! Codificação de chaves *memcomparable*: comparar os bytes (`memcmp`) dá a mesma
//! ordem que comparar os valores. É isso que deixa a B-tree trabalhar só com bytes,
//! sem conhecer tipos.
//!
//! * NULL < BOOL < números < TEXT (um tag de 1 byte separa os tipos)
//! * INT: inverte o bit de sinal e grava big-endian
//! * FLOAT: truque do IEEE (inverte tudo se negativo, senão só o bit de sinal)
//! * TEXT: bytes com `00` escapado como `00 FF` e terminador `00 01`, o que torna a
//!   codificação livre de prefixo (nenhuma chave é prefixo de outra)

use crate::heap::RowId;
use crate::value::Value;

/// Acrescenta a codificação *memcomparable* de `v` a `out`.
pub fn encode_value(v: &Value, out: &mut Vec<u8>) {
    match v {
        Value::Null => out.push(0x00),
        Value::Bool(b) => {
            out.push(0x01);
            out.push(*b as u8);
        }
        Value::Int(i) => {
            out.push(0x02);
            out.extend_from_slice(&((*i as u64) ^ (1 << 63)).to_be_bytes());
        }
        Value::Float(f) => {
            out.push(0x03);
            let bits = f.to_bits();
            let ordered = if bits >> 63 == 1 { !bits } else { bits | (1 << 63) };
            out.extend_from_slice(&ordered.to_be_bytes());
        }
        Value::Text(s) => {
            out.push(0x04);
            for &b in s.as_bytes() {
                out.push(b);
                if b == 0 {
                    out.push(0xFF);
                }
            }
            out.extend_from_slice(&[0x00, 0x01]);
        }
    }
}

/// Codificação *memcomparable* de um valor: a chave usada nos índices.
pub fn value_key(v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    encode_value(v, &mut out);
    out
}

/// Chave de índice não único: o `RowId` desempata entradas com o mesmo valor.
pub fn value_rid_key(v: &Value, rid: RowId) -> Vec<u8> {
    let mut out = value_key(v);
    out.extend_from_slice(&rid.page.to_be_bytes());
    out.extend_from_slice(&rid.slot.to_be_bytes());
    out
}

/// Primeira chave maior que qualquer entrada que comece com `key`
/// (o sufixo de um `RowId` tem 6 bytes, então 8 bytes `FF` bastam).
pub fn prefix_end(key: &[u8]) -> Vec<u8> {
    let mut out = key.to_vec();
    out.extend_from_slice(&[0xFF; 8]);
    out
}

/// Menor chave que não é NULL (índices incluem NULLs, mas comparações nunca os casam).
pub const AFTER_NULL: [u8; 1] = [0x01];

#[cfg(test)]
mod tests {
    use super::*;

    fn ordered(vals: &[Value]) {
        let keys: Vec<_> = vals.iter().map(value_key).collect();
        for w in keys.windows(2) {
            assert!(w[0] < w[1], "{w:?}");
        }
    }

    #[test]
    fn order_is_preserved() {
        ordered(&[Value::Int(i64::MIN), Value::Int(-5), Value::Int(0), Value::Int(7), Value::Int(i64::MAX)]);
        ordered(&[Value::Float(-1e300), Value::Float(-1.5), Value::Float(0.0), Value::Float(2.5), Value::Float(1e300)]);
        ordered(&[
            Value::Text("".into()),
            Value::Text("a".into()),
            Value::Text("a\0".into()),
            Value::Text("a\0b".into()),
            Value::Text("ab".into()),
            Value::Text("b".into()),
        ]);
        ordered(&[Value::Null, Value::Bool(false), Value::Bool(true), Value::Int(0), Value::Text("x".into())]);
    }
}
