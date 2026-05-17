//! Unicode / emoji palette provider (spec §2.1.2 / §2.6 — "Unicode/emoji
//! lookup").
//!
//! A curated table (not the full UCD — that would be megabytes and mostly
//! noise) of the symbols a researcher actually reaches for: Greek letters,
//! math/logic operators, set theory, arrows, and a handful of common
//! emoji. Search matches any keyword substring; selecting copies the
//! character to the Wayland clipboard (`wl-copy`).

use async_trait::async_trait;

use super::provider::{PaletteItem, PaletteProvider, ProviderError, ProviderResult};

pub const UNICODE_PROVIDER: &str = "unicode";

/// `(char, canonical name, extra keywords)`. The canonical name is shown
/// as the title; keywords broaden matching (e.g. "sum" → ∑).
const TABLE: &[(&str, &str, &str)] = &[
    // Greek lowercase
    ("α", "alpha", "greek"), ("β", "beta", "greek"), ("γ", "gamma", "greek"),
    ("δ", "delta", "greek"), ("ε", "epsilon", "greek"), ("ζ", "zeta", "greek"),
    ("η", "eta", "greek"), ("θ", "theta", "greek"), ("ι", "iota", "greek"),
    ("κ", "kappa", "greek"), ("λ", "lambda", "greek"), ("μ", "mu", "greek micro"),
    ("ν", "nu", "greek"), ("ξ", "xi", "greek"), ("π", "pi", "greek"),
    ("ρ", "rho", "greek"), ("σ", "sigma", "greek"), ("τ", "tau", "greek"),
    ("φ", "phi", "greek"), ("χ", "chi", "greek"), ("ψ", "psi", "greek"),
    ("ω", "omega", "greek"),
    // Greek uppercase
    ("Γ", "Gamma", "greek upper"), ("Δ", "Delta", "greek upper change"),
    ("Θ", "Theta", "greek upper"), ("Λ", "Lambda", "greek upper"),
    ("Π", "Pi", "greek upper product"), ("Σ", "Sigma", "greek upper sum"),
    ("Φ", "Phi", "greek upper"), ("Ψ", "Psi", "greek upper"),
    ("Ω", "Omega", "greek upper ohm"),
    // Math / logic
    ("∑", "summation", "sum sigma"), ("∏", "product", "prod pi"),
    ("∫", "integral", "calculus"), ("∂", "partial derivative", "del"),
    ("∇", "nabla", "gradient del"), ("√", "square root", "radical sqrt"),
    ("∞", "infinity", "inf"), ("≈", "approximately equal", "approx"),
    ("≠", "not equal", "neq"), ("≤", "less than or equal", "leq"),
    ("≥", "greater than or equal", "geq"), ("±", "plus minus", "pm"),
    ("×", "multiplication", "times cross"), ("÷", "division", "divide"),
    ("·", "middle dot", "cdot dot product"), ("∝", "proportional to", "prop"),
    ("∈", "element of", "in member"), ("∉", "not element of", "notin"),
    ("⊂", "subset of", "subset"), ("⊆", "subset or equal", "subseteq"),
    ("∪", "union", "set"), ("∩", "intersection", "set"),
    ("∅", "empty set", "emptyset null"), ("∀", "for all", "forall"),
    ("∃", "there exists", "exists"), ("¬", "logical not", "negation"),
    ("∧", "logical and", "wedge"), ("∨", "logical or", "vee"),
    ("⊕", "direct sum", "xor oplus"), ("⊗", "tensor product", "otimes"),
    ("→", "rightwards arrow", "arrow to implies"),
    ("←", "leftwards arrow", "arrow"), ("↔", "left right arrow", "iff"),
    ("⇒", "rightwards double arrow", "implies"),
    ("⇔", "left right double arrow", "iff equiv"),
    ("≡", "identical to", "equiv"), ("∴", "therefore", "qed"),
    ("∵", "because", "since"), ("ℝ", "real numbers", "reals"),
    ("ℕ", "natural numbers", "naturals"), ("ℤ", "integers", "ints"),
    ("ℚ", "rationals", "rationals"), ("ℂ", "complex numbers", "complex"),
    ("°", "degree", "degrees"), ("′", "prime", "minutes feet"),
    ("″", "double prime", "seconds inches"), ("ℓ", "script l", "liter"),
    // Common emoji
    ("✓", "check mark", "tick done yes"), ("✗", "ballot x", "cross no fail"),
    ("★", "star filled", "favorite"), ("☆", "star outline", "favorite"),
    ("⚠", "warning sign", "caution"), ("→", "arrow", "next"),
    ("🔥", "fire", "hot lit"), ("🚀", "rocket", "launch ship"),
    ("🐛", "bug", "debug defect"), ("✅", "check button", "done ok"),
    ("❌", "cross mark", "no fail wrong"), ("💡", "light bulb", "idea"),
    ("📌", "pushpin", "pin"), ("🎯", "target", "goal bullseye"),
    ("⏰", "alarm clock", "time reminder"), ("📝", "memo", "note write"),
];

pub struct UnicodeProvider;

impl UnicodeProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for UnicodeProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PaletteProvider for UnicodeProvider {
    fn name(&self) -> &'static str {
        UNICODE_PROVIDER
    }

    async fn search(&self, query: &str) -> Vec<PaletteItem> {
        let q = query.trim().to_lowercase();
        if q.len() < 2 {
            // Avoid flooding the merge on a single keystroke.
            return Vec::new();
        }
        let mut out = Vec::new();
        for (ch, name, kw) in TABLE {
            let hay = format!("{name} {kw}");
            if let Some(pos) = hay.find(&q) {
                // Earlier (esp. name-start) matches rank higher; keep
                // these below exact app/calc hits but above fuzzy text.
                let score = 0.70 - (pos as f64 * 0.01).min(0.25);
                out.push(
                    PaletteItem::new(UNICODE_PROVIDER, (*ch).to_string(),
                        format!("{ch}  {name}"))
                        .with_subtitle(format!("U+{:04X}  ·  copy",
                            ch.chars().next().map(|c| c as u32).unwrap_or(0)))
                        .with_icon("unicode")
                        .with_score(score),
                );
            }
            if out.len() >= 12 {
                break;
            }
        }
        out
    }

    async fn execute(&self, item_id: &str) -> ProviderResult<()> {
        super::spawn_detached("wl-copy", &[item_id])
            .map_err(|e| ProviderError::ExecuteFailed(format!("wl-copy: {e}")))?;
        tracing::info!(glyph = item_id, "unicode: copied glyph");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn matches_name_and_keyword() {
        let p = UnicodeProvider::new();
        let lam = p.search("lambda").await;
        assert!(lam.iter().any(|i| i.id == "λ"));
        // Keyword match: "sum" should surface Σ / ∑.
        let sum = p.search("sum").await;
        assert!(sum.iter().any(|i| i.id == "∑" || i.id == "Σ"));
    }

    #[tokio::test]
    async fn single_char_query_is_ignored() {
        assert!(UnicodeProvider::new().search("a").await.is_empty());
    }
}
