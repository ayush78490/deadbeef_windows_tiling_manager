//! fzf-style fuzzy matching: the query's characters must appear in order;
//! consecutive runs, word starts and prefix matches score higher.

/// Returns (score, matched char indices) or None if `query` isn't a subsequence.
pub fn score(query: &str, text: &str) -> Option<(i32, Vec<usize>)> {
    let q: Vec<char> = query.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    if q.is_empty() {
        return Some((0, Vec::new()));
    }
    let t: Vec<char> = text.chars().collect();
    let tl: Vec<char> = text.to_lowercase().chars().collect();
    if tl.len() != t.len() {
        return None; // exotic case folding; skip rather than mis-highlight
    }
    let word_start = |i: usize| -> bool {
        i == 0
            || matches!(t[i - 1], ' ' | '-' | '_' | '.' | '(' | '/' | '\\')
            || (t[i].is_uppercase() && t[i - 1].is_lowercase())
    };

    // Try every occurrence of the first query char as a starting point and
    // keep the best greedy match from there.
    let mut best: Option<(i32, Vec<usize>)> = None;
    for start in (0..tl.len()).filter(|&i| tl[i] == q[0]) {
        let mut idx = vec![start];
        let mut ti = start + 1;
        for &qc in &q[1..] {
            // Prefer the next word-start occurrence if it comes soon; else the nearest.
            let next = (ti..tl.len()).find(|&i| tl[i] == qc);
            let Some(n) = next else {
                idx.clear();
                break;
            };
            idx.push(n);
            ti = n + 1;
        }
        if idx.is_empty() {
            continue;
        }
        let mut s = 0i32;
        for (k, &i) in idx.iter().enumerate() {
            s += 16;
            if word_start(i) {
                s += 12;
            }
            if k > 0 && idx[k - 1] + 1 == i {
                s += 10;
            } else if k > 0 {
                s -= (i - idx[k - 1] - 1).min(8) as i32;
            }
        }
        if start == 0 {
            s += 20;
        }
        s -= (tl.len() as i32) / 8;
        if best.as_ref().is_none_or(|(b, _)| s > *b) {
            best = Some((s, idx));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_sensibly() {
        let s = |q, t| score(q, t).map(|x| x.0).unwrap_or(i32::MIN);
        assert!(s("vsc", "Visual Studio Code") > s("vsc", "Visual Studio Installer Cache"));
        assert!(s("code", "Visual Studio Code") > s("code", "Unicode Viewer"));
        assert!(s("term", "Terminal") > s("term", "Windows Terminal"));
        assert!(score("xyz", "Notepad").is_none());
        assert_eq!(score("np", "Notepad").unwrap().1, vec![0, 4]);
    }
}
