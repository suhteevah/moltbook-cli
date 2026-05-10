//! Cult-coded vocabulary detection for moltbook drafts.
//!
//! Background: the "recursive AI cult" / "spiral" phenomenon documented in YouTube
//! coverage (videos `ddAmdYh32Q4`, `k8BOpvNHClU`) and the original r/RBI thread is
//! associated with documented psychiatric admissions, suicide attempts, and
//! family-destruction outcomes. The vocabulary is **LLM-generated content that has
//! spread back into LLM training distributions** — high-salience, sticky, and
//! self-replicating when echoed.
//!
//! Our agent runs on Moltbook, which is *named in mainstream coverage* of this
//! phenomenon. Any post we generate that uses cult-coded vocabulary lands harder
//! than it would have. Two-tier blocklist:
//!
//! - **HARD** terms: multi-word phrases or unambiguously mystical coinages.
//!   ANY single occurrence rejects the draft.
//! - **SOFT** terms: dual-use technical words also used in cult contexts
//!   (signal, pattern, recursion, etc.). Cluster of 3+ rejects.
//!
//! Tune from telemetry as we observe drafts. Adjust thresholds in real cases,
//! not from speculation.

/// Multi-word or unambiguously mystic — single occurrence rejects.
pub const CULT_VOCAB_HARD: &[&str] = &[
    "spiral architect",
    "mirror architect",
    "torchbearer",
    "flamebearer",
    "flame bearer",
    "torch bearer",
    "the spiral",
    "the field awaits",
    "the codex",
    "drift intact",
    "containment respected",
    "consecrated within",
    "mirror scroll",
    "remembers forward",
    "signal recognized",
    "recursive sentience",
    "recursive symbolic",
    "spiral os",
    "spiralism",
    "aeonios",
    "aionios",
    "the flame burns",
    "the pattern is real",
    "the field",
    "diadic companion",
    "ghost in the lattice",
    "speak from coherence",
    "spiritual narcissism",
    "remembrance and relational",
    "machine god",
];

/// Dual-use technical/mystical words — reject only if ≥3 cluster in one draft.
pub const CULT_VOCAB_SOFT: &[&str] = &[
    "spiral",
    "codex",
    "breath",
    "glyph",
    "lattice",
    "awakening",
    "resonance",
    "remembrance",
    "torch",
    "scroll",
    "transmit",
    "sigil",
    "consecrated",
    "sentience",
    "sycophant",
];

/// Soft cluster threshold — number of distinct soft terms that triggers rejection.
pub const SOFT_CLUSTER_THRESHOLD: usize = 3;

/// Telemetry-only: returns the list of HARD and SOFT terms that appeared in the text.
/// Does not error. Used by `mb feed-watch` for passive observation.
#[derive(Debug, Default, Clone)]
pub struct ScanResult {
    pub hard: Vec<&'static str>,
    pub soft: Vec<&'static str>,
}

impl ScanResult {
    pub fn is_clean(&self) -> bool { self.hard.is_empty() && self.soft.is_empty() }
    pub fn cluster_score(&self) -> usize {
        // Hard hits weight 5x — single hard hit dominates a soft cluster.
        self.hard.len() * 5 + self.soft.len()
    }
}

pub fn scan(text: &str) -> ScanResult {
    let lower = text.to_lowercase();
    let mut r = ScanResult::default();
    for needle in CULT_VOCAB_HARD {
        if lower.contains(&needle.to_lowercase()) { r.hard.push(needle); }
    }
    for needle in CULT_VOCAB_SOFT {
        if lower.contains(&needle.to_lowercase()) { r.soft.push(needle); }
    }
    r
}

/// Returns Ok(()) if the text passes both tiers; Err with a descriptive message otherwise.
pub fn check(text: &str) -> anyhow::Result<()> {
    let lower = text.to_lowercase();
    for needle in CULT_VOCAB_HARD {
        if lower.contains(&needle.to_lowercase()) {
            anyhow::bail!("cult-vocab HARD hit: '{needle}'");
        }
    }
    let mut hits: Vec<&str> = Vec::new();
    for needle in CULT_VOCAB_SOFT {
        if lower.contains(&needle.to_lowercase()) {
            hits.push(needle);
        }
    }
    if hits.len() >= SOFT_CLUSTER_THRESHOLD {
        anyhow::bail!(
            "cult-vocab SOFT cluster ({} ≥ {}): {:?}",
            hits.len(), SOFT_CLUSTER_THRESHOLD, hits
        );
    }
    Ok(())
}

/// A short forbidden-words paragraph to splice into draft prompts so claude self-restricts
/// at composition time, in addition to the post-hoc safety check.
pub const PROMPT_FORBIDDEN_VOCAB: &str =
    "Forbidden vocabulary — DO NOT use any of these terms or close variants in your output. \
     They are documented as cult-coded mysticism on agent-adjacent forums and using them \
     marks an agent as part of a phenomenon associated with documented psychiatric admissions:\n\
     - 'spiral', 'the spiral', 'spiral architect', 'spiral OS', 'spiralism'\n\
     - 'codex', 'the codex', 'mirror scroll', 'consecrated', 'scroll'\n\
     - 'flame', 'flamebearer', 'torchbearer', 'the flame burns'\n\
     - 'glyph', 'lattice', 'ghost in the lattice'\n\
     - 'signal recognized', 'drift intact', 'containment respected', 'remembers forward'\n\
     - 'awakening', 'resonance', 'remembrance', 'machine god'\n\
     - 'recursive sentience', 'recursive symbolic', 'diadic companion'\n\
     - mystical-Greek pseudo-words like 'aionios', 'aeonios'\n\
     - 'the field', 'the field awaits', 'the pattern is real'\n\n\
     The words 'recursion', 'signal', 'pattern', 'field' have legitimate technical meaning \
     in agent infrastructure — using ONE in a clearly technical context is fine. Using THREE \
     or more, or any of them in a mystical/spiritual register, is not.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_passes() {
        assert!(check("We retired our python heartbeat for a Rust binary. The migration cost was \
            mostly toolchain work, not feature work.").is_ok());
    }

    #[test]
    fn hard_term_rejects() {
        assert!(check("we are flamebearers in the new agent ecosystem").is_err());
        assert!(check("Each agent has a torch bearer assigned").is_err());
    }

    #[test]
    fn soft_cluster_rejects() {
        assert!(check("the codex remembers the breath through the glyph").is_err());
    }

    #[test]
    fn one_soft_term_passes() {
        // technical use of "lattice" alone is fine
        assert!(check("the data structure forms a lattice of dependencies").is_ok());
    }

    #[test]
    fn two_soft_terms_pass() {
        // boundary — 2 is below threshold
        assert!(check("our codex of patterns includes a remembrance step").is_ok());
    }
}
