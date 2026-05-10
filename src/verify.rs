//! Solver for Moltbook's "lobster math" verification challenges.
//!
//! Challenges look like: "A] lOoO bS-tEr S^wImS lIkE aN eXpErImEnTaL lOoObSsStErr WiTh
//! VeLaWcItEe O f tWeNtY tHrEe CeNtImEtErS PeR sEcOnD ] aNd AnOtHeR lOoO bSsStErr WiTh
//! VeLaWcItEe O f FoUrTeEn CeNtImEtErS PeR sEcOnD, wHaT iS ThEiR ToTaL VeLoAwCiTy?"
//!
//! Strategy mirrors the existing python heartbeat solver:
//!  1. Strip non-alpha, lowercase, collapse 3+ repeated chars (looooo -> lo).
//!  2. Run a vocabulary-greedy pass over the deduped blob, also over a "collapse all
//!     doubles" variant — pick whichever yields more number-word hits.
//!  3. Compose adjacent number words into integer values.
//!  4. Detect the operation from a small keyword vocabulary (sum, product, diff, ratio).
//!  5. Compute and format with two decimals.

use anyhow::{anyhow, Result};
use std::collections::HashMap;

fn number_words() -> HashMap<&'static str, i64> {
    let mut m = HashMap::new();
    for (w, v) in [
        ("zero", 0), ("one", 1), ("two", 2), ("three", 3), ("four", 4),
        ("five", 5), ("six", 6), ("seven", 7), ("eight", 8), ("nine", 9),
        ("ten", 10), ("eleven", 11), ("twelve", 12), ("thirteen", 13),
        ("fourteen", 14), ("fifteen", 15), ("sixteen", 16), ("seventeen", 17),
        ("eighteen", 18), ("nineteen", 19), ("twenty", 20), ("thirty", 30),
        ("forty", 40), ("fifty", 50), ("sixty", 60), ("seventy", 70),
        ("eighty", 80), ("ninety", 90), ("hundred", 100),
    ] {
        m.insert(w, v);
    }
    m
}

fn variant_fixups(s: &str) -> String {
    let pairs: &[(&str, &str)] = &[
        ("fiften", "fifteen"), ("fiftten", "fifteen"), ("thirten", "thirteen"),
        ("fourten", "fourteen"), ("ninteen", "nineteen"), ("eightteen", "eighteen"),
        ("tweny", "twenty"), ("thrity", "thirty"), ("fourty", "forty"),
        ("fivety", "fifty"), ("ninty", "ninety"), ("elevent", "eleven"),
    ];
    let mut out = s.to_string();
    for (k, v) in pairs {
        out = out.replace(k, v);
    }
    out
}

/// Collapse runs of 3+ identical characters down to 1 (looooo -> lo). Keeps doubles intact.
fn collapse_runs_3plus(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let mut run = 1;
        while i + run < chars.len() && chars[i + run] == c {
            run += 1;
        }
        if run >= 3 {
            out.push(c);
        } else {
            for _ in 0..run { out.push(c); }
        }
        i += run;
    }
    out
}

fn collapse_all_doubles(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        out.push(chars[i]);
        if i + 1 < chars.len() && chars[i + 1] == chars[i] {
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

fn extract_words<'a>(blob: &str, vocab: &'a [&'a str]) -> Vec<&'a str> {
    let mut result = Vec::new();
    let bytes = blob.as_bytes();
    let mut j = 0;
    while j < bytes.len() {
        let mut matched = false;
        for word in vocab {
            let wb = word.as_bytes();
            if j + wb.len() <= bytes.len() && &bytes[j..j + wb.len()] == wb {
                result.push(*word);
                j += wb.len();
                matched = true;
                break;
            }
        }
        if !matched { j += 1; }
    }
    result
}

fn compose_numbers(words: &[&str], nw: &HashMap<&'static str, i64>) -> Vec<i64> {
    let mut out = Vec::new();
    let mut current: i64 = 0;
    let mut in_number = false;
    for w in words {
        if let Some(&val) = nw.get(*w) {
            if val == 100 {
                current = if current == 0 { 100 } else { current * 100 };
            } else if val >= 20 {
                if in_number && current > 0 && current < 20 {
                    out.push(current);
                    current = val;
                } else {
                    current += val;
                }
            } else {
                current += val;
            }
            in_number = true;
        } else if in_number && current > 0 {
            out.push(current);
            current = 0;
            in_number = false;
        }
    }
    if current > 0 { out.push(current); }
    out
}

fn pick_operation(words: &[&str]) -> Op {
    let mut sum_score = 0;
    let mut prod_score = 0;
    let mut diff_score = 0;
    for w in words {
        match *w {
            "sum" | "total" | "plus" | "add" | "and" => sum_score += 1,
            "product" | "multiply" | "multiplies" | "times" => prod_score += 1,
            "difference" | "minus" | "subtract" => diff_score += 1,
            _ => {}
        }
    }
    if prod_score > sum_score && prod_score > diff_score { Op::Product }
    else if diff_score > sum_score && diff_score > prod_score { Op::Difference }
    else { Op::Sum }
}

#[derive(Debug, Clone, Copy)]
enum Op { Sum, Product, Difference }

/// Solve a lobster-math challenge. Returns the answer formatted as "N.00".
pub fn solve(challenge_text: &str) -> Result<String> {
    let nw = number_words();
    let lowered: String = challenge_text.chars()
        .filter(|c| c.is_ascii_alphabetic() || c.is_whitespace())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let collapsed_3plus = collapse_runs_3plus(&lowered);
    let blob_a: String = collapsed_3plus.chars().filter(|c| !c.is_whitespace()).collect();
    let blob_a = variant_fixups(&blob_a);
    let blob_b = collapse_all_doubles(&blob_a);

    let nw_words: Vec<&str> = {
        let mut v: Vec<&str> = nw.keys().copied().collect();
        v.sort_by_key(|s| std::cmp::Reverse(s.len()));
        v
    };
    let extra: &[&str] = &[
        "lobster", "lobsters", "swims", "swim", "claw", "claws", "force", "newtons",
        "newton", "grip", "molting", "molt", "during", "and", "per", "seconds",
        "second", "what", "the", "sum", "product", "multiply", "multiplies",
        "plus", "minus", "difference", "total", "add", "subtract",
        "centimeters", "centimeter", "meters", "meter", "millimeters", "millimeter",
        "times", "with", "exerts", "exert", "its", "at", "is", "cm", "mm", "um",
        "are", "their", "experimental", "another", "velocity", "an", "a",
    ];
    let mut all_vocab: Vec<&str> = nw_words.iter().copied().chain(extra.iter().copied()).collect();
    all_vocab.sort_by_key(|s| std::cmp::Reverse(s.len()));

    let ex1 = extract_words(&blob_a, &all_vocab);
    let ex2 = extract_words(&blob_b, &all_vocab);
    let nums1 = ex1.iter().filter(|w| nw.contains_key(*w)).count();
    let nums2 = ex2.iter().filter(|w| nw.contains_key(*w)).count();
    let words = if nums2 > nums1 { ex2 } else { ex1 };

    let numbers = compose_numbers(&words, &nw);
    if numbers.is_empty() {
        return Err(anyhow!("no numbers extracted from challenge: {}", challenge_text));
    }
    if numbers.len() == 1 {
        return Ok(format!("{}.00", numbers[0]));
    }
    let op = pick_operation(&words);
    let result: i64 = match op {
        Op::Sum => numbers.iter().sum(),
        Op::Product => numbers.iter().product(),
        Op::Difference => {
            let mut it = numbers.iter();
            let first = *it.next().unwrap();
            it.fold(first, |acc, x| acc - x)
        }
    };
    Ok(format!("{}.00", result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_amsi_challenge() {
        // 23 + 14 = 37
        let c = "A] lOoO bS-tEr S^wImS lIkE aN eXpErImEnTaL lOoObSsStErr WiTh VeLaWcItEe O f tWeNtY tHrEe CeNtImEtErS PeR sEcOnD ] aNd AnOtHeR lOoO bSsStErr WiTh VeLaWcItEe O f FoUrTeEn CeNtImEtErS PeR sEcOnD, wHaT iS ThEiR ToTaL VeLoAwCiTy?";
        assert_eq!(solve(c).unwrap(), "37.00");
    }
}
