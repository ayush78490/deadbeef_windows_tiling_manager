//! Tiny calculator for the launcher: + - * / % ^, parentheses, unary minus,
//! constants (pi, e) and functions (sqrt abs ln log sin cos tan round floor ceil).

/// Evaluates `s` if it looks like arithmetic (a leading `=` forces it).
pub fn eval(s: &str) -> Option<f64> {
    let forced = s.trim_start().starts_with('=');
    let s = s.trim().trim_start_matches('=').trim();
    if s.is_empty() {
        return None;
    }
    let looks_math = s.chars().any(|c| c.is_ascii_digit()) && (forced || s.chars().any(|c| "+-*/%^(".contains(c)));
    if !looks_math {
        return None;
    }
    let mut p = Parser { s: s.as_bytes(), i: 0 };
    let v = p.expr()?;
    p.ws();
    (p.i == p.s.len() && v.is_finite()).then_some(v)
}

pub fn format(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.10}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        loop {
            if self.eat(b'+') {
                v += self.term()?;
            } else if self.eat(b'-') {
                v -= self.term()?;
            } else {
                return Some(v);
            }
        }
    }
    fn term(&mut self) -> Option<f64> {
        let mut v = self.power()?;
        loop {
            if self.eat(b'*') || self.eat(b'x') {
                v *= self.power()?;
            } else if self.eat(b'/') {
                v /= self.power()?;
            } else if self.eat(b'%') {
                v %= self.power()?;
            } else {
                return Some(v);
            }
        }
    }
    fn power(&mut self) -> Option<f64> {
        let base = self.unary()?;
        if self.eat(b'^') {
            Some(base.powf(self.power()?))
        } else {
            Some(base)
        }
    }
    fn unary(&mut self) -> Option<f64> {
        if self.eat(b'-') {
            return Some(-self.unary()?);
        }
        if self.eat(b'+') {
            return self.unary();
        }
        self.atom()
    }
    fn atom(&mut self) -> Option<f64> {
        self.ws();
        if self.eat(b'(') {
            let v = self.expr()?;
            return self.eat(b')').then_some(v);
        }
        let start = self.i;
        if self.s.get(self.i).is_some_and(|c| c.is_ascii_digit() || *c == b'.') {
            while self.s.get(self.i).is_some_and(|c| c.is_ascii_digit() || *c == b'.' || *c == b'_') {
                self.i += 1;
            }
            let txt: String = std::str::from_utf8(&self.s[start..self.i]).ok()?.replace('_', "");
            return txt.parse().ok();
        }
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_alphabetic()) {
            self.i += 1;
        }
        let name = std::str::from_utf8(&self.s[start..self.i]).ok()?.to_ascii_lowercase();
        match name.as_str() {
            "pi" => Some(std::f64::consts::PI),
            "e" => Some(std::f64::consts::E),
            "" => None,
            f => {
                if !self.eat(b'(') {
                    return None;
                }
                let a = self.expr()?;
                if !self.eat(b')') {
                    return None;
                }
                Some(match f {
                    "sqrt" => a.sqrt(),
                    "abs" => a.abs(),
                    "ln" => a.ln(),
                    "log" => a.log10(),
                    "sin" => a.to_radians().sin(),
                    "cos" => a.to_radians().cos(),
                    "tan" => a.to_radians().tan(),
                    "round" => a.round(),
                    "floor" => a.floor(),
                    "ceil" => a.ceil(),
                    _ => return None,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates() {
        assert_eq!(eval("12*8+3"), Some(99.0));
        assert_eq!(eval("2^3^2"), Some(512.0));
        assert_eq!(eval("-(4 - 10) / 4"), Some(1.5));
        assert_eq!(eval("=sqrt(16) + pi - pi"), Some(4.0));
        assert_eq!(eval("10 % 4"), Some(2.0));
        assert_eq!(eval("notepad"), None);
        assert_eq!(eval("7zip"), None); // a digit but no operator: an app name
        assert_eq!(eval("1/0"), None);
        assert_eq!(format(0.1 + 0.2), "0.3");
        assert_eq!(format(99.0), "99");
    }
}
