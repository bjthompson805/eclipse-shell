//! The calculator: "=" evaluates the rest; a plain query that reads as
//! arithmetic gets an answer row too. Enter copies the result.
//!
//! A small recursive-descent evaluator: + - * / % ^ (right-associative),
//! postfix !, parentheses, unary minus, pi/e/tau, and the usual functions
//! (sqrt, cbrt, abs, sin, cos, tan, asin, acos, atan, ln, log, log2, exp,
//! floor, ceil, round). Trig works in radians.

use std::rc::Rc;

use crate::launcher::results::{Entry, Icon};

pub fn evaluate(text: &str) -> Option<f64> {
    let mut parser = Parser { chars: text.chars().filter(|c| !c.is_whitespace()).collect(), at: 0 };
    let value = parser.expression()?;
    (parser.at == parser.chars.len() && value.is_finite()).then_some(value)
}

/// Whether a plain query is worth evaluating: a digit and an operator.
pub fn looks_like_math(text: &str) -> bool {
    text.chars().any(|c| c.is_ascii_digit()) && text.chars().any(|c| "+-*/^%!(".contains(c)) && evaluate(text).is_some()
}

pub fn format(value: f64) -> String {
    if value == value.trunc() && value.abs() < 1e15 {
        return format!("{}", value as i64);
    }
    if value.abs() >= 1e15 || value.abs() < 1e-6 {
        return format!("{value:.6e}");
    }
    let text = format!("{value:.10}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

pub fn entry(expression: &str) -> Option<Entry> {
    let value = evaluate(expression)?;
    let result = format(value);
    let copied = result.clone();
    Some(Entry {
        icon: Icon::Glyph("calculate"),
        title: result,
        subtitle: format!("{} · Enter copies", expression.trim()),
        matched: Vec::new(),
        verb: "Copy",
        action: Rc::new(move || crate::audio::spawn(&["wl-copy", &copied])),
    })
}

struct Parser {
    chars: Vec<char>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    /// sum: term (("+" | "-") term)*
    fn expression(&mut self) -> Option<f64> {
        let mut value = self.term()?;
        loop {
            if self.eat('+') {
                value += self.term()?;
            } else if self.eat('-') {
                value -= self.term()?;
            } else {
                return Some(value);
            }
        }
    }

    /// term: unary (("*" | "/" | "%") unary)*
    fn term(&mut self) -> Option<f64> {
        let mut value = self.unary()?;
        loop {
            if self.eat('*') || self.eat('×') {
                value *= self.unary()?;
            } else if self.eat('/') || self.eat('÷') {
                value /= self.unary()?;
            } else if self.eat('%') {
                value %= self.unary()?;
            } else {
                return Some(value);
            }
        }
    }

    /// unary: "-" unary | power
    fn unary(&mut self) -> Option<f64> {
        if self.eat('-') {
            return Some(-self.unary()?);
        }
        if self.eat('+') {
            return self.unary();
        }
        self.power()
    }

    /// power: postfix ("^" unary)?, right-associative.
    fn power(&mut self) -> Option<f64> {
        let base = self.postfix()?;
        if self.eat('^') {
            return Some(base.powf(self.unary()?));
        }
        Some(base)
    }

    /// postfix: atom "!"*
    fn postfix(&mut self) -> Option<f64> {
        let mut value = self.atom()?;
        while self.eat('!') {
            if value < 0.0 || value != value.trunc() || value > 170.0 {
                return None;
            }
            value = (1..=value as u64).map(|n| n as f64).product();
        }
        Some(value)
    }

    fn atom(&mut self) -> Option<f64> {
        if self.eat('(') {
            let value = self.expression()?;
            return self.eat(')').then_some(value);
        }
        let c = self.peek()?;
        if c.is_ascii_digit() || c == '.' {
            let start = self.at;
            while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '.') {
                self.at += 1;
            }
            // An exponent: 1e3, 2.5e-4.
            if self.peek() == Some('e') && self.chars.get(self.at + 1).is_some_and(|c| c.is_ascii_digit() || *c == '-' || *c == '+') {
                self.at += 2;
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.at += 1;
                }
            }
            let text: String = self.chars[start..self.at].iter().collect();
            return text.parse().ok();
        }
        if c.is_alphabetic() || c == 'π' {
            let start = self.at;
            while self.peek().is_some_and(|c| c.is_alphanumeric() || c == 'π') {
                self.at += 1;
            }
            let name: String = self.chars[start..self.at].iter().collect::<String>().to_lowercase();
            match name.as_str() {
                "pi" | "π" => return Some(std::f64::consts::PI),
                "e" => return Some(std::f64::consts::E),
                "tau" => return Some(std::f64::consts::TAU),
                _ => {}
            }
            let f: fn(f64) -> f64 = match name.as_str() {
                "sqrt" => f64::sqrt,
                "cbrt" => f64::cbrt,
                "abs" => f64::abs,
                "sin" => f64::sin,
                "cos" => f64::cos,
                "tan" => f64::tan,
                "asin" => f64::asin,
                "acos" => f64::acos,
                "atan" => f64::atan,
                "ln" => f64::ln,
                "log" => f64::log10,
                "log2" => f64::log2,
                "exp" => f64::exp,
                "floor" => f64::floor,
                "ceil" => f64::ceil,
                "round" => f64::round,
                _ => return None,
            };
            let argument = if self.eat('(') {
                let value = self.expression()?;
                if !self.eat(')') {
                    return None;
                }
                value
            } else {
                self.postfix()?
            };
            return Some(f(argument));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic() {
        assert_eq!(evaluate("2+2*3"), Some(8.0));
        assert_eq!(evaluate("(2+2)*3"), Some(12.0));
        assert_eq!(evaluate("2^3^2"), Some(512.0));
        assert_eq!(evaluate("-2^2"), Some(-4.0));
        assert_eq!(evaluate("10 % 4"), Some(2.0));
        assert_eq!(evaluate("5!"), Some(120.0));
        assert_eq!(evaluate("1e3/4"), Some(250.0));
        assert_eq!(evaluate("sqrt(16) + abs(-2)"), Some(6.0));
        assert!((evaluate("sin(pi/2)").unwrap() - 1.0).abs() < 1e-12);
        assert_eq!(evaluate("2+"), None);
        assert_eq!(evaluate("1/0"), None);
        assert_eq!(evaluate("firefox"), None);
    }

    #[test]
    fn formats() {
        assert_eq!(format(8.0), "8");
        assert_eq!(format(0.1 + 0.2), "0.3");
        assert_eq!(format(1.0 / 3.0), "0.3333333333");
    }

    #[test]
    fn spots_math() {
        assert!(looks_like_math("12*4"));
        assert!(!looks_like_math("firefox"));
        assert!(!looks_like_math("2048"));
    }
}
