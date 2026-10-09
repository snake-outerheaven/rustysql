//! Lexer: texto SQL → tokens.

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Ident(String),
    Int(i64),
    Float(f64),
    Str(String),
    LParen,
    RParen,
    Comma,
    Semi,
    Star,
    Plus,
    Minus,
    Slash,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

pub fn tokenize(input: &str) -> Result<Vec<Token>> {
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();

    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            c if c.is_whitespace() => i += 1,
            '-' if next == Some('-') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '(' => { out.push(Token::LParen); i += 1 }
            ')' => { out.push(Token::RParen); i += 1 }
            ',' => { out.push(Token::Comma); i += 1 }
            ';' => { out.push(Token::Semi); i += 1 }
            '*' => { out.push(Token::Star); i += 1 }
            '+' => { out.push(Token::Plus); i += 1 }
            '-' => { out.push(Token::Minus); i += 1 }
            '/' => { out.push(Token::Slash); i += 1 }
            '=' => { out.push(Token::Eq); i += 1 }
            '!' if next == Some('=') => { out.push(Token::Ne); i += 2 }
            '<' if next == Some('>') => { out.push(Token::Ne); i += 2 }
            '<' if next == Some('=') => { out.push(Token::Le); i += 2 }
            '>' if next == Some('=') => { out.push(Token::Ge); i += 2 }
            '<' => { out.push(Token::Lt); i += 1 }
            '>' => { out.push(Token::Gt); i += 1 }
            '\'' => {
                i += 1;
                let mut s = String::new();
                loop {
                    match chars.get(i) {
                        None => return Err(Error::parse("string sem fechar")),
                        Some('\'') if chars.get(i + 1) == Some(&'\'') => { s.push('\''); i += 2 }
                        Some('\'') => { i += 1; break }
                        Some(&ch) => { s.push(ch); i += 1 }
                    }
                }
                out.push(Token::Str(s));
            }
            c if c.is_ascii_digit() => {
                let start = i;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let mut is_float = false;
                if chars.get(i) == Some(&'.') && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit()) {
                    is_float = true;
                    i += 1;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let text: String = chars[start..i].iter().collect();
                out.push(if is_float {
                    Token::Float(text.parse().map_err(|_| Error::parse("número inválido"))?)
                } else {
                    Token::Int(text.parse().map_err(|_| Error::parse("inteiro fora do intervalo"))?)
                });
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                out.push(Token::Ident(chars[start..i].iter().collect()));
            }
            other => return Err(Error::parse(format!("caractere inesperado '{other}'"))),
        }
    }
    Ok(out)
}
