//! Calculator + unit-conversion palette provider (spec §2.1.2 /
//! §2.6 — "calculator and unit/currency conversion").
//!
//! Two query shapes are recognised:
//!
//! * **Arithmetic**: `2 + 2`, `(3.5 * 4) ^ 2`, `17 % 5`, `-2^10`.
//!   A small recursive-descent f64 evaluator (`+ - * / % ^`, unary
//!   minus, parentheses, right-assoc `^`).
//! * **Unit conversion**: `10 km to mi`, `72 f in c`, `1.5 GiB to MiB`.
//!   Curated dimensions: length, mass, temperature, digital storage.
//!
//! Currency is intentionally **not** implemented: a correct answer needs
//! a live FX source, and a stale/fake rate is worse than no result. It's
//! tracked for a later networked pass rather than shipped wrong.
//!
//! Selecting the result copies it to the Wayland clipboard (`wl-copy`).

use async_trait::async_trait;

use super::provider::{PaletteItem, PaletteProvider, ProviderError, ProviderResult};

pub const CALC_PROVIDER: &str = "calc";

pub struct CalcProvider;

impl CalcProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CalcProvider {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------
// Arithmetic evaluator (recursive descent over a char tokeniser).
// ---------------------------------------------------------------------

struct Parser<'a> {
    s: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            s: src.chars().peekable(),
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.s.peek(), Some(c) if c.is_whitespace()) {
            self.s.next();
        }
    }

    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        loop {
            self.skip_ws();
            match self.s.peek() {
                Some('+') => {
                    self.s.next();
                    v += self.term()?;
                }
                Some('-') => {
                    self.s.next();
                    v -= self.term()?;
                }
                _ => return Some(v),
            }
        }
    }

    fn term(&mut self) -> Option<f64> {
        let mut v = self.unary()?;
        loop {
            self.skip_ws();
            match self.s.peek() {
                Some('*') => {
                    self.s.next();
                    v *= self.unary()?;
                }
                Some('/') => {
                    self.s.next();
                    let d = self.unary()?;
                    if d == 0.0 {
                        return None;
                    }
                    v /= d;
                }
                Some('%') => {
                    self.s.next();
                    let d = self.unary()?;
                    if d == 0.0 {
                        return None;
                    }
                    v %= d;
                }
                _ => return Some(v),
            }
        }
    }

    // Unary minus binds *looser* than `^` (math convention:
    // `-2^10 == -(2^10)`), so unary wraps power, not the other way round.
    fn unary(&mut self) -> Option<f64> {
        self.skip_ws();
        match self.s.peek() {
            Some('-') => {
                self.s.next();
                Some(-self.unary()?)
            }
            Some('+') => {
                self.s.next();
                self.unary()
            }
            _ => self.power(),
        }
    }

    fn power(&mut self) -> Option<f64> {
        let base = self.atom()?;
        self.skip_ws();
        if self.s.peek() == Some(&'^') {
            self.s.next();
            // Right-associative; the exponent may itself be signed
            // (`2 ^ -3`), so recurse through `unary`.
            let exp = self.unary()?;
            return Some(base.powf(exp));
        }
        Some(base)
    }

    fn atom(&mut self) -> Option<f64> {
        self.skip_ws();
        if self.s.peek() == Some(&'(') {
            self.s.next();
            let v = self.expr()?;
            self.skip_ws();
            if self.s.next() != Some(')') {
                return None;
            }
            return Some(v);
        }
        let mut num = String::new();
        while let Some(&c) = self.s.peek() {
            if c.is_ascii_digit() || c == '.' {
                num.push(c);
                self.s.next();
            } else {
                break;
            }
        }
        num.parse::<f64>().ok()
    }
}

/// Evaluate a pure arithmetic expression. Returns `None` if the string
/// isn't a complete, well-formed expression (so plain words don't get
/// mistaken for math).
fn eval_arith(src: &str) -> Option<f64> {
    let mut p = Parser::new(src);
    let v = p.expr()?;
    p.skip_ws();
    // Reject trailing garbage: "2 + 2 foo" must not evaluate.
    if p.s.peek().is_some() {
        return None;
    }
    v.is_finite().then_some(v)
}

// ---------------------------------------------------------------------
// Unit conversion.
// ---------------------------------------------------------------------

/// Linear-dimension table: canonical-unit factor per alias. Temperature
/// is handled separately (affine, not a simple factor).
fn linear_factor(unit: &str) -> Option<(&'static str, f64)> {
    // (dimension tag, factor to the dimension's base unit)
    let u = unit.to_ascii_lowercase();
    let t: &[(&str, &str, f64)] = &[
        // length → metre
        ("len", "mm", 0.001), ("len", "cm", 0.01), ("len", "m", 1.0),
        ("len", "km", 1000.0), ("len", "in", 0.0254), ("len", "ft", 0.3048),
        ("len", "yd", 0.9144), ("len", "mi", 1609.344),
        // mass → gram
        ("mass", "mg", 0.001), ("mass", "g", 1.0), ("mass", "kg", 1000.0),
        ("mass", "lb", 453.59237), ("mass", "oz", 28.349523125),
        // digital → byte
        ("data", "b", 1.0), ("data", "kb", 1000.0), ("data", "mb", 1.0e6),
        ("data", "gb", 1.0e9), ("data", "tb", 1.0e12),
        ("data", "kib", 1024.0), ("data", "mib", 1048576.0),
        ("data", "gib", 1073741824.0), ("data", "tib", 1099511627776.0),
    ];
    t.iter()
        .find(|(_, name, _)| *name == u)
        .map(|(dim, _, f)| (*dim, *f))
}

fn to_celsius(v: f64, unit: &str) -> Option<f64> {
    match unit.to_ascii_lowercase().as_str() {
        "c" | "celsius" => Some(v),
        "f" | "fahrenheit" => Some((v - 32.0) * 5.0 / 9.0),
        "k" | "kelvin" => Some(v - 273.15),
        _ => None,
    }
}
fn from_celsius(c: f64, unit: &str) -> Option<f64> {
    match unit.to_ascii_lowercase().as_str() {
        "c" | "celsius" => Some(c),
        "f" | "fahrenheit" => Some(c * 9.0 / 5.0 + 32.0),
        "k" | "kelvin" => Some(c + 273.15),
        _ => None,
    }
}

/// Parse `<number> <from> (to|in) <to>` and convert. Returns the value
/// plus the target unit label for display.
fn eval_conversion(src: &str) -> Option<(f64, String)> {
    let lower = src.to_lowercase();
    let sep = [" to ", " in ", " into "]
        .iter()
        .find_map(|s| lower.find(s).map(|i| (i, s.len())))?;
    let (lhs, _) = src.split_at(sep.0);
    let rhs = src[sep.0 + sep.1..].trim();
    let to_unit = rhs.split_whitespace().next()?;

    let lhs = lhs.trim();
    // Split the numeric prefix from the from-unit (e.g. "1.5GiB" or
    // "1.5 GiB").
    let split = lhs
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))?;
    let value: f64 = lhs[..split].trim().parse().ok()?;
    let from_unit = lhs[split..].trim();

    // Temperature first (affine).
    if let (Some(c), true) = (to_celsius(value, from_unit), from_celsius(0.0, to_unit).is_some()) {
        return from_celsius(c, to_unit).map(|v| (v, to_unit.to_string()));
    }
    // Linear dimensions: both units must share a dimension.
    let (fd, ff) = linear_factor(from_unit)?;
    let (td, tf) = linear_factor(to_unit)?;
    if fd != td {
        return None;
    }
    Some((value * ff / tf, to_unit.to_string()))
}

fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        // Trim to 6 sig-ish digits without trailing zeros.
        let s = format!("{v:.6}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

#[async_trait]
impl PaletteProvider for CalcProvider {
    fn name(&self) -> &'static str {
        CALC_PROVIDER
    }

    async fn search(&self, query: &str) -> Vec<PaletteItem> {
        let q = query.trim();
        if q.is_empty() {
            return Vec::new();
        }
        let (result_text, kind) = if let Some((v, unit)) = eval_conversion(q) {
            (format!("{} {}", fmt_num(v), unit), "conversion")
        } else if let Some(v) = eval_arith(q) {
            (fmt_num(v), "result")
        } else {
            return Vec::new();
        };
        // Top of the merge when it parses — a calculator answer is an
        // exact, intentional query.
        vec![PaletteItem::new(CALC_PROVIDER, result_text.clone(), result_text)
            .with_subtitle(format!("{kind}  ·  {q}"))
            .with_icon("calc")
            .with_score(0.99)]
    }

    async fn execute(&self, item_id: &str) -> ProviderResult<()> {
        super::spawn_detached("wl-copy", &[item_id])
            .map_err(|e| ProviderError::ExecuteFailed(format!("wl-copy: {e}")))?;
        tracing::info!(result = item_id, "calc: copied result");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic() {
        assert_eq!(eval_arith("2 + 2"), Some(4.0));
        assert_eq!(eval_arith("(3.5 * 4) ^ 2"), Some(196.0));
        assert_eq!(eval_arith("17 % 5"), Some(2.0));
        assert_eq!(eval_arith("-2 ^ 10"), Some(-1024.0)); // unary binds looser than ^
        assert_eq!(eval_arith("2 ^ 3 ^ 2"), Some(512.0)); // right-assoc
        assert_eq!(eval_arith("10 / 4"), Some(2.5));
    }

    #[test]
    fn non_math_is_rejected() {
        assert_eq!(eval_arith("firefox"), None);
        assert_eq!(eval_arith("2 + 2 foo"), None);
        assert_eq!(eval_arith("1 / 0"), None);
        assert_eq!(eval_arith(""), None);
    }

    #[test]
    fn conversions() {
        assert_eq!(eval_conversion("10 km to mi").map(|(v, _)| v.round()), Some(6.0));
        let (c, u) = eval_conversion("212 f in c").unwrap();
        assert!((c - 100.0).abs() < 1e-9);
        assert_eq!(u, "c");
        let (g, _) = eval_conversion("2 gib to mib").unwrap();
        assert!((g - 2048.0).abs() < 1e-6);
        // Cross-dimension is rejected.
        assert!(eval_conversion("10 km to kg").is_none());
    }

    #[tokio::test]
    async fn search_emits_single_top_ranked_item() {
        let p = CalcProvider::new();
        let items = p.search("2+2*3").await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "8");
        assert!(items[0].score > 0.9);
        assert!(p.search("just words").await.is_empty());
    }
}
