//! Paragraph-aware text chunking. Targets ~1,200 chars per chunk with ~200
//! chars of overlap carried between consecutive chunks so context isn't lost
//! at boundaries. All sizes are in chars (not bytes) — safe for unicode.

pub const CHUNK_TARGET: usize = 1200;
pub const CHUNK_OVERLAP: usize = 200;

/// Content-type-aware chunking — SuperMemory's "Super RAG" idea: the split
/// strategy follows the content. Markdown splits on heading hierarchy (so a
/// section stays whole and is tagged with its heading), meeting transcripts
/// split on speaker turns, everything else falls back to paragraph packing.
/// `smart` off forces the plain paragraph path (for A/B isolation).
pub fn chunk_doc(content: &str, source: &str, file_path: Option<&str>, smart: bool) -> Vec<String> {
    if !smart {
        return chunk_text(content, CHUNK_TARGET, CHUNK_OVERLAP);
    }
    let is_md = file_path
        .map(|p| {
            let p = p.to_lowercase();
            p.ends_with(".md") || p.ends_with(".markdown") || p.ends_with(".mdx")
        })
        .unwrap_or(false);
    if source == "meeting" {
        chunk_transcript(content, CHUNK_TARGET, CHUNK_OVERLAP)
    } else if is_md || looks_like_markdown(content) {
        chunk_markdown(content, CHUNK_TARGET, CHUNK_OVERLAP)
    } else {
        chunk_text(content, CHUNK_TARGET, CHUNK_OVERLAP)
    }
}

/// Heuristic: several Markdown headings present even without a .md extension
/// (browser-captured articles, pasted docs).
fn looks_like_markdown(text: &str) -> bool {
    text.lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("# ") || t.starts_with("## ") || t.starts_with("### ")
        })
        .count()
        >= 2
}

/// Split `text` into chunks of roughly `target` chars. Paragraphs are kept
/// whole when possible; oversized paragraphs split at sentence boundaries,
/// hard-cut as a last resort. Each chunk after the first starts with the last
/// `overlap` chars of its predecessor.
pub fn chunk_text(text: &str, target: usize, overlap: usize) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return vec![];
    }
    if text.chars().count() <= target {
        return vec![text.to_string()];
    }

    // 1. Break into paragraph-or-smaller pieces, none longer than `target`.
    let mut pieces: Vec<String> = Vec::new();
    for p in text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        if p.chars().count() <= target {
            pieces.push(p.to_string());
        } else {
            pieces.extend(split_long(p, target));
        }
    }

    pack_pieces(pieces, target, overlap)
}

/// Pack already-bounded pieces (each ≤ target) into chunks, seeding each new
/// chunk with the overlap tail of its predecessor. Shared by every strategy.
fn pack_pieces(pieces: Vec<String>, target: usize, overlap: usize) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut cur = String::new();
    for piece in &pieces {
        if !cur.is_empty() && cur.chars().count() + piece.chars().count() + 2 > target {
            let tail = char_tail(&cur, overlap);
            chunks.push(std::mem::take(&mut cur));
            cur = tail;
        }
        if !cur.is_empty() {
            cur.push_str("\n\n");
        }
        cur.push_str(piece);
    }
    if !cur.trim().is_empty() {
        chunks.push(cur);
    }
    chunks
}

/// Markdown chunking: break on heading lines so each section is a unit, and
/// prefix every section with the heading trail (e.g. "# Guide ▸ ## Setup") so
/// a chunk carries its location in the document. Oversized sections split by
/// sentence; small adjacent sections pack together up to target.
pub fn chunk_markdown(text: &str, target: usize, overlap: usize) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return vec![];
    }
    // Track the current heading trail (level → text) so each section knows its path.
    let mut trail: Vec<(usize, String)> = Vec::new();
    let mut pieces: Vec<String> = Vec::new();
    let mut body = String::new();

    let flush = |trail: &[(usize, String)], body: &str, pieces: &mut Vec<String>| {
        let b = body.trim();
        if b.is_empty() {
            return;
        }
        let crumb = trail
            .iter()
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>()
            .join(" ▸ ");
        let section = if crumb.is_empty() {
            b.to_string()
        } else {
            format!("{crumb}\n{b}")
        };
        if section.chars().count() <= target {
            pieces.push(section);
        } else {
            pieces.extend(split_long(&section, target));
        }
    };

    for line in text.lines() {
        let t = line.trim_start();
        let level = t.chars().take_while(|c| *c == '#').count();
        if level > 0 && level <= 6 && t.chars().nth(level) == Some(' ') {
            // New heading: flush the prior section, then update the trail.
            flush(&trail, &body, &mut pieces);
            body.clear();
            trail.retain(|(l, _)| *l < level);
            trail.push((level, t.to_string()));
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    flush(&trail, &body, &mut pieces);

    if pieces.is_empty() {
        return chunk_text(text, target, overlap);
    }
    pack_pieces(pieces, target, overlap)
}

/// Transcript chunking: keep speaker turns intact. A turn starts at a line like
/// "Alex:" / "[00:12] Alex:" / "Speaker 1:". Turns pack together up to target;
/// long monologues split by sentence. Falls back to paragraph chunking when no
/// speaker labels are detected.
pub fn chunk_transcript(text: &str, target: usize, overlap: usize) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return vec![];
    }
    let mut turns: Vec<String> = Vec::new();
    let mut cur = String::new();
    for line in text.lines() {
        if is_speaker_line(line) && !cur.trim().is_empty() {
            turns.push(std::mem::take(&mut cur));
        }
        cur.push_str(line);
        cur.push('\n');
    }
    if !cur.trim().is_empty() {
        turns.push(cur);
    }
    if turns.len() < 2 {
        return chunk_text(text, target, overlap); // no real speaker structure
    }
    let mut pieces: Vec<String> = Vec::new();
    for turn in turns {
        let turn = turn.trim().to_string();
        if turn.chars().count() <= target {
            pieces.push(turn);
        } else {
            pieces.extend(split_long(&turn, target));
        }
    }
    pack_pieces(pieces, target, overlap)
}

/// A line that opens a speaker turn: "Name:" or "[timestamp] Name:" with a
/// short label before the colon (not prose that merely contains a colon).
fn is_speaker_line(line: &str) -> bool {
    let l = line.trim_start();
    // strip a leading [..] or (..) timestamp
    let l = l
        .strip_prefix('[')
        .and_then(|r| r.split_once(']'))
        .map(|(_, r)| r.trim_start())
        .unwrap_or(l);
    let Some((label, _rest)) = l.split_once(':') else {
        return false;
    };
    let label = label.trim();
    !label.is_empty()
        && label.chars().count() <= 32
        && label
            .chars()
            .next()
            .map(|c| c.is_uppercase() || c.is_ascii_digit())
            .unwrap_or(false)
        && label.split_whitespace().count() <= 3
        && !label.contains(['.', '!', '?', ','])
}

/// Split an oversized paragraph at sentence-ish boundaries; hard-cut any
/// single sentence longer than `target`.
fn split_long(p: &str, target: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for sentence in p.split_inclusive(['.', '!', '?', '\n']) {
        let s_len = sentence.chars().count();
        if !cur.is_empty() && cur.chars().count() + s_len > target {
            out.push(cur.trim().to_string());
            cur = String::new();
        }
        if s_len > target {
            let chars: Vec<char> = sentence.chars().collect();
            for window in chars.chunks(target) {
                out.push(window.iter().collect::<String>().trim().to_string());
            }
        } else {
            cur.push_str(sentence);
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out.retain(|s| !s.is_empty());
    out
}

/// Last `n` chars of `s` (char-boundary safe).
fn char_tail(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        return s.to_string();
    }
    s.chars().skip(count - n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_one_chunk() {
        let chunks = chunk_text("hello world", 1200, 200);
        assert_eq!(chunks, vec!["hello world".to_string()]);
    }

    #[test]
    fn empty_text_is_no_chunks() {
        assert!(chunk_text("   \n\n  ", 1200, 200).is_empty());
    }

    #[test]
    fn long_text_chunks_respect_target() {
        let para = "The quick brown fox jumps over the lazy dog. ".repeat(20); // ~920 chars
        let text = vec![para; 6].join("\n\n");
        let chunks = chunk_text(&text, 1200, 200);
        assert!(
            chunks.len() >= 3,
            "expected several chunks, got {}",
            chunks.len()
        );
        for c in &chunks {
            // target + overlap seed + joining slack
            assert!(
                c.chars().count() <= 1200 + 200 + 4,
                "chunk too big: {}",
                c.chars().count()
            );
        }
    }

    #[test]
    fn consecutive_chunks_overlap() {
        let para = "Sentence one is here. ".repeat(120); // ~2640 chars, one paragraph
        let chunks = chunk_text(&para, 1200, 200);
        assert!(chunks.len() >= 2);
        let tail: String = chunks[0]
            .chars()
            .skip(chunks[0].chars().count().saturating_sub(50))
            .collect();
        assert!(
            chunks[1].contains(tail.trim()),
            "second chunk should contain the tail of the first"
        );
    }

    #[test]
    fn pathological_unbroken_text_hard_cuts() {
        let blob = "x".repeat(5000);
        let chunks = chunk_text(&blob, 1200, 200);
        assert!(chunks.len() >= 4);
        for c in &chunks {
            // target + overlap seed + joining slack
            assert!(
                c.chars().count() <= 1200 + 200 + 4,
                "chunk too big: {}",
                c.chars().count()
            );
        }
    }

    #[test]
    fn unicode_does_not_panic() {
        let text = "日本語のテキスト。".repeat(400);
        let chunks = chunk_text(&text, 1200, 200);
        assert!(!chunks.is_empty());
    }

    #[test]
    fn markdown_sections_carry_heading_trail() {
        let md = "# Guide\nintro text\n\n## Setup\ninstall the thing\n\n## Usage\nrun the thing";
        let chunks = chunk_markdown(md, 1200, 200);
        // Small sections pack into one chunk here; the heading crumbs must appear.
        let joined = chunks.join("\n");
        assert!(
            joined.contains("# Guide"),
            "heading trail missing: {joined}"
        );
        assert!(joined.contains("## Setup"));
        assert!(joined.contains("install the thing"));
    }

    #[test]
    fn markdown_large_sections_split_but_keep_path() {
        let big = "word ".repeat(400); // ~2000 chars under one heading
        let md = format!("# Big\n## Section A\n{big}");
        let chunks = chunk_markdown(&md, 1200, 200);
        assert!(chunks.len() >= 2, "large section should split");
        assert!(chunks[0].contains("Section A"));
    }

    #[test]
    fn markdown_without_headings_falls_back() {
        let plain = "just some text\n\nwith two paragraphs";
        let chunks = chunk_markdown(plain, 1200, 200);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].contains("two paragraphs"));
    }

    #[test]
    fn transcript_splits_on_speaker_turns() {
        let t = "Alex: Welcome everyone to the standup today.\nJordan: Thanks, I finished the API work.\nAlex: Great, what is next on your list?";
        assert!(is_speaker_line("Alex: hello"));
        assert!(is_speaker_line("[00:12] Jordan: hi"));
        assert!(!is_speaker_line("This is a sentence: with a colon."));
        let chunks = chunk_transcript(t, 80, 10); // tiny target → one chunk per turn-ish
        assert!(
            chunks.len() >= 2,
            "expected multiple speaker chunks, got {}",
            chunks.len()
        );
    }

    #[test]
    fn chunk_doc_routes_by_type() {
        // Markdown by extension.
        let md = "# Title\nbody one\n\n## Two\nbody two";
        let smart = chunk_doc(md, "file", Some("/x/readme.md"), true);
        assert!(smart.join("").contains("# Title"));
        // smart=false forces plain paragraph path (no heading crumb prefix logic).
        let plain = chunk_doc(md, "file", Some("/x/readme.md"), false);
        assert_eq!(plain, chunk_text(md, CHUNK_TARGET, CHUNK_OVERLAP));
    }
}
