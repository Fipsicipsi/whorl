//! Hand-written lexer + recursive-descent parser for the P2 language.
//! Zero dependencies, line-tracked for source-accurate witnesses.
//!
//! The lexer decodes by `char` (never by raw byte) so it cannot panic on
//! multi-byte UTF-8, and block nesting is depth-bounded so deeply nested input
//! produces a clean error instead of a stack overflow.

use crate::ast::{Ast, Func, Item, Stmt};

/// Maximum block-nesting depth. Past this the parser returns an error rather
/// than recursing into a stack overflow.
const MAX_PARSE_DEPTH: usize = 512;

#[derive(Clone, Debug, PartialEq)]
enum Tk {
    Ident(String),
    Colon,
    LBrace,
    RBrace,
    LParen,
    RParen,
    Comma,
}

struct Token {
    tk: Tk,
    line: usize,
}

fn lex(src: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    let mut line = 1;

    while i < chars.len() {
        let c = chars[i];
        match c {
            '\n' => {
                line += 1;
                i += 1;
            }
            c if c.is_whitespace() => i += 1,
            '/' if i + 1 < chars.len() && chars[i + 1] == '/' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            ':' => {
                toks.push(Token {
                    tk: Tk::Colon,
                    line,
                });
                i += 1;
            }
            '{' => {
                toks.push(Token {
                    tk: Tk::LBrace,
                    line,
                });
                i += 1;
            }
            '}' => {
                toks.push(Token {
                    tk: Tk::RBrace,
                    line,
                });
                i += 1;
            }
            '(' => {
                toks.push(Token {
                    tk: Tk::LParen,
                    line,
                });
                i += 1;
            }
            ')' => {
                toks.push(Token {
                    tk: Tk::RParen,
                    line,
                });
                i += 1;
            }
            ',' => {
                toks.push(Token {
                    tk: Tk::Comma,
                    line,
                });
                i += 1;
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let s: String = chars[start..i].iter().collect();
                toks.push(Token {
                    tk: Tk::Ident(s),
                    line,
                });
            }
            _ => return Err(format!("line {line}: unexpected character '{c}'")),
        }
    }
    Ok(toks)
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn line(&self) -> usize {
        self.toks.get(self.pos).map(|t| t.line).unwrap_or(0)
    }

    fn at_end(&self) -> bool {
        self.pos >= self.toks.len()
    }

    fn peek_is_kw(&self, kw: &str) -> bool {
        matches!(self.toks.get(self.pos), Some(Token { tk: Tk::Ident(s), .. }) if s == kw)
    }

    fn peek_is(&self, t: &Tk) -> bool {
        matches!(self.toks.get(self.pos), Some(tok) if &tok.tk == t)
    }

    fn peek_next_is(&self, t: &Tk) -> bool {
        matches!(self.toks.get(self.pos + 1), Some(tok) if &tok.tk == t)
    }

    fn expect(&mut self, want: Tk) -> Result<(), String> {
        match self.toks.get(self.pos) {
            Some(t) if t.tk == want => {
                self.pos += 1;
                Ok(())
            }
            Some(t) => Err(format!(
                "line {}: expected {:?}, found {:?}",
                t.line, want, t.tk
            )),
            None => Err(format!("unexpected end of input, expected {want:?}")),
        }
    }

    fn eat_kw(&mut self, kw: &str) -> Result<(), String> {
        if self.peek_is_kw(kw) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("line {}: expected '{}'", self.line(), kw))
        }
    }

    fn eat_ident(&mut self) -> Result<(usize, String), String> {
        match self.toks.get(self.pos) {
            Some(Token {
                tk: Tk::Ident(s),
                line,
            }) => {
                let out = (*line, s.clone());
                self.pos += 1;
                Ok(out)
            }
            Some(t) => Err(format!("line {}: expected an identifier", t.line)),
            None => Err("unexpected end of input, expected an identifier".to_string()),
        }
    }

    /// Parse a parenthesised, comma-separated list of identifiers: `( a, b, c )`
    /// or `( )`. Used for both function parameters and call arguments.
    fn ident_list(&mut self) -> Result<Vec<String>, String> {
        self.expect(Tk::LParen)?;
        let mut out = Vec::new();
        if !self.peek_is(&Tk::RParen) {
            let (_, first) = self.eat_ident()?;
            out.push(first);
            while self.peek_is(&Tk::Comma) {
                self.expect(Tk::Comma)?;
                let (_, x) = self.eat_ident()?;
                out.push(x);
            }
        }
        self.expect(Tk::RParen)?;
        Ok(out)
    }

    fn program(&mut self) -> Result<Ast, String> {
        let mut items = Vec::new();
        while !self.at_end() {
            if self.peek_is_kw("lock") {
                items.push(self.lock_decl()?);
            } else if self.peek_is_kw("extern") {
                items.push(self.extern_decl()?);
            } else if self.peek_is_kw("isr") {
                self.eat_kw("isr")?;
                items.push(self.fn_decl(true)?);
            } else if self.peek_is_kw("fn") {
                items.push(self.fn_decl(false)?);
            } else {
                return Err(format!(
                    "line {}: expected 'lock', 'fn', 'isr', or 'extern'",
                    self.line()
                ));
            }
        }
        Ok(Ast { items })
    }

    fn extern_decl(&mut self) -> Result<Item, String> {
        self.eat_kw("extern")?;
        self.eat_kw("fn")?;
        let (_, name) = self.eat_ident()?;
        let mut acquires = Vec::new();
        if self.peek_is_kw("acquires") {
            self.eat_kw("acquires")?;
            let (_, first) = self.eat_ident()?;
            acquires.push(first);
            while self.peek_is(&Tk::Comma) {
                self.expect(Tk::Comma)?;
                let (_, c) = self.eat_ident()?;
                acquires.push(c);
            }
        }
        Ok(Item::Extern { name, acquires })
    }

    fn lock_decl(&mut self) -> Result<Item, String> {
        self.eat_kw("lock")?;
        let (_, name) = self.eat_ident()?;
        self.expect(Tk::Colon)?;
        let (_, class) = self.eat_ident()?;
        Ok(Item::Lock { name, class })
    }

    fn fn_decl(&mut self, is_isr: bool) -> Result<Item, String> {
        self.eat_kw("fn")?;
        let (_, name) = self.eat_ident()?;
        let params = self.ident_list()?;
        let body = self.block(1)?;
        Ok(Item::Func(Func {
            name,
            params,
            body,
            is_isr,
        }))
    }

    fn block(&mut self, depth: usize) -> Result<Vec<Stmt>, String> {
        if depth > MAX_PARSE_DEPTH {
            return Err(format!(
                "line {}: block nesting deeper than {} is not supported",
                self.line(),
                MAX_PARSE_DEPTH
            ));
        }
        self.expect(Tk::LBrace)?;
        let mut stmts = Vec::new();
        while !self.peek_is(&Tk::RBrace) {
            if self.at_end() {
                return Err("unexpected end of input inside a block".to_string());
            }
            stmts.push(self.stmt(depth)?);
        }
        self.expect(Tk::RBrace)?;
        Ok(stmts)
    }

    fn stmt(&mut self, depth: usize) -> Result<Stmt, String> {
        // `couple`/`with` are keywords only when not immediately followed by `(`
        // - so a function named `couple` is still callable via `couple()`.
        if self.peek_is_kw("couple") && !self.peek_next_is(&Tk::LParen) {
            let line = self.line();
            self.eat_kw("couple")?;
            let (_, class) = self.eat_ident()?;
            let body = self.block(depth + 1)?;
            return Ok(Stmt::Couple { class, line, body });
        }
        if self.peek_is_kw("mask") && self.peek_next_is(&Tk::LBrace) {
            self.eat_kw("mask")?;
            let body = self.block(depth + 1)?;
            return Ok(Stmt::Mask { body });
        }
        if self.peek_is_kw("with") && !self.peek_next_is(&Tk::LParen) {
            let line = self.line();
            self.eat_kw("with")?;
            if self.peek_is_kw("ordered") {
                self.eat_kw("ordered")?;
                let locks = self.ident_list()?;
                if locks.is_empty() {
                    return Err(format!("line {line}: ordered(...) needs at least one lock"));
                }
                let body = self.block(depth + 1)?;
                Ok(Stmt::Ordered { locks, line, body })
            } else {
                let (_, lock) = self.eat_ident()?;
                let body = self.block(depth + 1)?;
                Ok(Stmt::With { lock, line, body })
            }
        } else {
            // A call: IDENT ( args )
            let (line, callee) = self.eat_ident()?;
            let args = self.ident_list()?;
            Ok(Stmt::Call { callee, args, line })
        }
    }
}

/// Parse P2 source into an AST.
pub fn parse(src: &str) -> Result<Ast, String> {
    let toks = lex(src)?;
    let mut p = Parser { toks, pos: 0 };
    p.program()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_ascii_identifier_does_not_panic() {
        let _ = parse("lock café : Resource\nfn f() { with café { } }");
    }

    #[test]
    fn deep_nesting_errors_cleanly() {
        let src = format!(
            "lock a: A\nfn t() {{ {}{} }}",
            "with a { ".repeat(2000),
            "}".repeat(2000)
        );
        assert!(parse(&src).is_err());
    }

    #[test]
    fn parses_params_and_args() {
        let ast = parse("fn helper(body) { body() }\nfn run() { helper(run) }").unwrap();
        assert_eq!(ast.items.len(), 2);
    }

    #[test]
    fn couple_and_with_are_callable_names() {
        // `couple`/`with` followed by `(` parse as calls, not keywords.
        parse("fn couple() { }\nfn g() { couple() }").expect("couple() should parse as a call");
        parse("fn with() { }\nfn g() { with() }").expect("with() should parse as a call");
    }
}
