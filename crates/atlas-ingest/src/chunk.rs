//! Intra-document chunking. Ported from graphify `file_slice.py`.
//!
//! Splits an oversized text document into contiguous, gap-free chunks at the
//! strongest nearby boundary — markdown heading (`\n#`), then blank line
//! (`\n\n`), then bare newline (`\n`) — so concatenating the chunks reproduces
//! the input exactly and no content is dropped. Each chunk stays within a rough
//! token budget.
//!
//! The budget is in *tokens*; we approximate tokens as `chars / CHARS_PER_TOKEN`
//! (the common ~4-chars-per-token rule of thumb) and slice on a char budget of
//! `budget * CHARS_PER_TOKEN`, so every chunk's estimated token count is within
//! budget. ponytail: char/4 heuristic, swap in a real tokenizer only if a
//! downstream model rejects a chunk as over-budget.

/// Rough chars-per-token used to turn a token budget into a char budget.
pub const CHARS_PER_TOKEN: usize = 4;

/// Boundary preferences, strongest first (mirrors graphify's
/// `_BOUNDARY_SEPARATORS`): a heading keeps a section with its title, a blank
/// line keeps a paragraph intact, a bare newline avoids cutting mid-line.
const SEPARATORS: [&[char]; 3] = [&['\n', '#'], &['\n', '\n'], &['\n']];

/// A contiguous piece of a document, ready for the semantic pass.
/// `source_location` is a graphify-style line range, e.g. `"L1-L12"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
    pub source_file: String,
    pub source_location: String,
}

/// Estimated token count for a string (the same heuristic the splitter uses).
pub fn estimate_tokens(s: &str) -> usize {
    s.chars().count().div_ceil(CHARS_PER_TOKEN)
}

/// Split `s` into chunks each within `budget` tokens, covering the whole input.
///
/// `source_file` is empty; callers that know the on-disk path fill it in. Chunks
/// are gap-free and non-overlapping: concatenating their `text` reproduces `s`.
pub fn chunk_text(s: &str, budget: usize) -> Vec<Chunk> {
    let max_chars = budget.saturating_mul(CHARS_PER_TOKEN).max(1);
    let chars: Vec<char> = s.chars().collect();
    let newline_prefix = line_index(&chars);
    slice_boundaries(&chars, max_chars)
        .into_iter()
        .map(|(start, end)| {
            let start_line = newline_prefix[start] + 1;
            // Line of the last character in the range (end is exclusive).
            let end_line = newline_prefix[end.saturating_sub(1).max(start)] + 1;
            Chunk {
                text: chars[start..end].iter().collect(),
                source_file: String::new(),
                source_location: format!("L{start_line}-L{end_line}"),
            }
        })
        .collect()
}

/// `newline_prefix[i]` = number of '\n' chars strictly before index `i`, i.e.
/// the 0-based line the char at `i` sits on. Length is `chars.len() + 1`.
fn line_index(chars: &[char]) -> Vec<usize> {
    let mut prefix = Vec::with_capacity(chars.len() + 1);
    let mut count = 0;
    prefix.push(0);
    for &c in chars {
        if c == '\n' {
            count += 1;
        }
        prefix.push(count);
    }
    prefix
}

/// Contiguous `(start, end)` char ranges covering all of `chars`, each ≤
/// `max_chars`. Faithful port of graphify `slice_boundaries`.
fn slice_boundaries(chars: &[char], max_chars: usize) -> Vec<(usize, usize)> {
    let n = chars.len();
    if n <= max_chars {
        return vec![(0, n)];
    }
    let mut bounds = Vec::new();
    let mut pos = 0;
    while pos < n {
        let hard = (pos + max_chars).min(n);
        let mut end = if hard < n {
            best_cut(chars, pos, hard)
        } else {
            n
        };
        if end <= pos {
            end = hard; // defensive: never stall
        }
        bounds.push((pos, end));
        pos = end;
    }
    bounds
}

/// A cut index in `(start, end]` at the strongest nearby boundary. Port of
/// graphify `_best_cut` (a heading cuts just *before* the `#` so it leads the
/// next chunk); falls back to a hard cut at `end`.
fn best_cut(chars: &[char], start: usize, end: usize) -> usize {
    let window = &chars[start..end];
    for sep in SEPARATORS {
        if let Some(idx) = rfind_seq(window, sep) {
            if idx > 0 {
                // A heading cuts just *before* the '#' so it leads the next
                // chunk (keep the newline with the previous chunk). The heading
                // separator is the only 2-char one.
                if sep == SEPARATORS[0] {
                    return start + idx + 1;
                }
                return start + idx + sep.len();
            }
        }
    }
    end
}

/// Index of the last occurrence of `sep` within `window`, or None.
fn rfind_seq(window: &[char], sep: &[char]) -> Option<usize> {
    if sep.is_empty() || sep.len() > window.len() {
        return None;
    }
    (0..=window.len() - sep.len())
        .rev()
        .find(|&i| &window[i..i + sep.len()] == sep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_whole_input_within_budget() {
        let doc = "\
# Title

First paragraph with several words spread across a couple of lines of text.

## Section two

Second paragraph here, also reasonably long so the splitter has to cut it.

### Section three

Third and final paragraph rounding out the document with more filler words.";
        let budget = 12; // tokens -> 48 char budget
        let chunks = chunk_text(doc, budget);

        assert!(chunks.len() > 1, "a doc past budget must split");
        // Every chunk within budget.
        for c in &chunks {
            assert!(
                estimate_tokens(&c.text) <= budget,
                "chunk over budget ({} tok): {:?}",
                estimate_tokens(&c.text),
                c.text
            );
        }
        // Gap-free, lossless coverage.
        let rejoined: String = chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(rejoined, doc, "chunks must reproduce the input exactly");
        // Prefer heading boundaries: a chunk after the first should start with '#'.
        assert!(chunks[1..].iter().any(|c| c.text.starts_with('#')));
    }

    #[test]
    fn short_input_is_one_chunk() {
        let chunks = chunk_text("tiny", 100);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "tiny");
        assert_eq!(chunks[0].source_location, "L1-L1");
    }

    #[test]
    fn unicode_boundaries_are_safe() {
        // Multibyte chars must not panic the char-index slicer.
        let doc = "café ☕ ".repeat(50);
        let chunks = chunk_text(&doc, 5);
        let rejoined: String = chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(rejoined, doc);
    }
}
