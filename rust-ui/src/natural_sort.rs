//! Port of `backend/natural_sort.py`: digit runs compare numerically, other text case-insensitively.
use std::cmp::Ordering;

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug)]
enum Part {
    // Python compares str with int only if layouts differ; split() always alternates text,digits,text… so the
    // derive order (text before number) is never exercised across positions.
    Text(String),
    Num(u128),
}

fn key(s: &str) -> Vec<Part> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut digits = false;
    let flush = |cur: &mut String, digits: bool, out: &mut Vec<Part>| {
        if digits {
            // ponytail: digit runs > 38 digits saturate to u128::MAX; irrelevant for frame names.
            out.push(Part::Num(cur.parse().unwrap_or(u128::MAX)));
        } else {
            out.push(Part::Text(cur.to_lowercase()));
        }
        cur.clear();
    };
    // Python's split always yields a leading text chunk (maybe empty).
    for ch in s.chars() {
        let d = ch.is_ascii_digit();
        if d != digits {
            flush(&mut cur, digits, &mut out);
            digits = d;
        }
        cur.push(ch);
    }
    flush(&mut cur, digits, &mut out);
    out
}

pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    key(a).cmp(&key(b))
}

pub fn natsort(items: &mut [String]) {
    items.sort_by(|a, b| natural_cmp(a, b));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numeric_runs() {
        let mut v: Vec<String> = ["f10", "f2", "f1", "F3"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        natsort(&mut v);
        assert_eq!(v, ["f1", "f2", "F3", "f10"]);
    }
    #[test]
    fn padded_and_unpadded_mix() {
        assert_eq!(natural_cmp("frame_000009", "frame_000010"), Ordering::Less);
        assert_eq!(natural_cmp("frame_9", "frame_000010"), Ordering::Less);
    }
}
