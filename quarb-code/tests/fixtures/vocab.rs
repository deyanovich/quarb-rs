mod lexer {
    /// Scan the input.
    pub fn lex(input: &str) -> Vec<char> {
        fn is_name_char(c: char) -> bool {
            c.is_alphanumeric() || c == '_'
        }
        let mut out = Vec::new();
        for c in input.chars() {
            if is_name_char(c) {
                out.push(c);
            } else if c == ' ' {
                out.push('_');
            } else {
                match c {
                    '\n' => out.push('.'),
                    _ => {}
                }
            }
        }
        out
    }
}

/// A cursor over the input.
struct Lexer {
    pos: usize,
}

enum Token {
    Word,
    Space,
}

impl Lexer {
    fn helper(&mut self, n: usize) -> usize {
        let f = |x: usize| x + 1;
        while self.pos < n {
            self.pos = f(self.pos);
        }
        self.pos
    }
}

fn run(n: usize) -> usize {
    let mut lx = Lexer { pos: 0 };
    lx.helper(n)
}

const LIMIT: usize = 10;

use std::fmt;
