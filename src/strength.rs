//! Password strength estimation (zxcvbn) in plain language.
//!
//! Note: the estimator keeps fragments of the password in ordinary heap
//! memory while it works; those are freed but cannot be wiped by us.

use zxcvbn::matching::patterns::MatchPattern;

#[derive(Clone, Debug, PartialEq)]
pub struct Strength {
    /// 0 (very weak) to 4 (very strong).
    pub score: u8,
    /// log2 of the estimated number of guesses.
    pub guess_bits: f64,
    /// Time to crack with a throttled online attack (100 guesses an hour).
    pub online: String,
    /// Time to crack offline against a slow hash (10,000 guesses a second).
    pub offline_slow: String,
    /// Time to crack offline against a fast hash (10 billion guesses a second).
    pub offline_fast: String,
    pub warning: Option<String>,
    pub suggestions: Vec<String>,
    /// Kinds of patterns found, in plain words.
    pub patterns: Vec<String>,
}

/// Longer inputs are estimated on their first characters only (the
/// estimator's cost grows quickly with length, and the answer no longer changes).
const MAX_CHARS: usize = 100;

pub fn estimate(password: &str, user_inputs: &[&str]) -> Strength {
    if password.is_empty() {
        return Strength {
            score: 0,
            guess_bits: 0.0,
            online: String::new(),
            offline_slow: String::new(),
            offline_fast: String::new(),
            warning: None,
            suggestions: Vec::new(),
            patterns: Vec::new(),
        };
    }
    let end = password
        .char_indices()
        .nth(MAX_CHARS)
        .map(|(i, _)| i)
        .unwrap_or(password.len());
    let e = zxcvbn::zxcvbn(&password[..end], user_inputs);
    let times = e.crack_times();
    let mut patterns: Vec<String> = Vec::new();
    for m in e.sequence() {
        let name = match &m.pattern {
            MatchPattern::Dictionary(d) => {
                if d.l33t {
                    "a common word with letter-for-symbol swaps"
                } else if d.reversed {
                    "a common word spelled backwards"
                } else {
                    "a common word or name"
                }
            }
            MatchPattern::Spatial(_) => "a keyboard pattern",
            MatchPattern::Repeat(_) => "repeated characters",
            MatchPattern::Sequence(_) => "a sequence such as abc or 123",
            MatchPattern::Regex(_) => "a recent year",
            MatchPattern::Date(_) => "a date",
            _ => continue,
        };
        if !patterns.iter().any(|p| p == name) {
            patterns.push(name.to_string());
        }
    }
    let (warning, suggestions) = match e.feedback() {
        Some(f) => (
            f.warning().map(|w| w.to_string()),
            f.suggestions().iter().map(|s| s.to_string()).collect(),
        ),
        None => (None, Vec::new()),
    };
    Strength {
        score: u8::from(e.score()),
        guess_bits: e.guesses_log10() * std::f64::consts::LOG2_10,
        online: times.online_throttling_100_per_hour().to_string(),
        offline_slow: times.offline_slow_hashing_1e4_per_second().to_string(),
        offline_fast: times.offline_fast_hashing_1e10_per_second().to_string(),
        warning,
        suggestions,
        patterns,
    }
}

impl Strength {
    /// The estimator counts guesses in 64 bits, so it cannot report more.
    pub fn capped(&self) -> bool {
        self.guess_bits >= 63.5
    }

    /// "about 41 bits", or "over 64 bits" at the estimator's ceiling.
    pub fn bits_text(&self) -> String {
        if self.capped() {
            "over 64 bits".to_string()
        } else {
            format!("about {:.0} bits", self.guess_bits)
        }
    }
}

pub fn score_label(score: u8) -> &'static str {
    match score {
        0 => "Very weak",
        1 => "Weak",
        2 => "Fair",
        3 => "Good",
        _ => "Strong",
    }
}

/// The lowest score accepted for a master password.
pub const MASTER_MIN_SCORE: u8 = 2;
/// The shortest master password accepted.
pub const MASTER_MIN_CHARS: usize = 8;

/// Why a master password is refused, if it is.
pub fn master_refusal(password: &str, s: &Strength) -> Option<&'static str> {
    if password.chars().count() < MASTER_MIN_CHARS {
        Some("Use at least 8 characters. A few random words work well.")
    } else if s.score < MASTER_MIN_SCORE {
        Some("This password is too easy to guess to protect a vault. Make it longer or less predictable.")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weak_and_strong() {
        let weak = estimate("password", &[]);
        assert_eq!(weak.score, 0);
        assert!(weak.warning.is_some());
        assert!(weak.patterns.iter().any(|p| p.contains("common word")));
        assert!(master_refusal("password", &weak).is_some());

        let strong = estimate("stoop-unrated-blizzard-ferret-payable", &[]);
        assert_eq!(strong.score, 4);
        assert!(strong.guess_bits > 60.0);
        assert_eq!(strong.bits_text(), "over 64 bits");
        assert!(weak.bits_text().starts_with("about "));
        assert!(master_refusal("stoop-unrated-blizzard-ferret-payable", &strong).is_none());

        assert!(["6tfcvgy7", "xdr5tgbnji9", "mko0nji9bhu8"]
            .iter()
            .any(|pw| estimate(pw, &[])
                .patterns
                .iter()
                .any(|p| p.contains("keyboard"))));
        assert!(estimate("aaaaaaaaaa", &[])
            .patterns
            .iter()
            .any(|p| p.contains("repeated")));
        assert_eq!(estimate("", &[]).score, 0);
        assert!(master_refusal("Xk9$", &estimate("Xk9$", &[])).is_some());
        // Very long input does not stall.
        let long = "x7Q!".repeat(2000);
        let started = std::time::Instant::now();
        estimate(&long, &[]);
        assert!(started.elapsed().as_secs() < 20);
        assert_eq!(score_label(4), "Strong");
    }
}
