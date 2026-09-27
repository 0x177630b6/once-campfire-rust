//! gzip for pages made mostly of cached fragments (a room's messages), without recompressing
//! the fragments on every request.
//!
//! The decoded body is exactly what [`super::gzip_stream`] would produce; only the compressed
//! bytes differ, as they already do from Ruby's zlib. A fragment is compressed once, against the
//! fragment before it as a preset dictionary, and the result is kept for as long as the fragment
//! is: deflate back-references can reach anything in the last 32 KB of output, so the piece is
//! valid wherever the same predecessor (and the same bytes between them) comes right before it,
//! which is how a room page repeats from one request to the next. Compressing each fragment on
//! its own would lose what consecutive messages share and make a room page ~4× larger; chained
//! this way it's ~15% larger. Everything else (the layout, the first fragment of each run) is
//! compressed live, at the same level, with the output before it as its dictionary.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, Weak};

use bytes::Bytes;
use flate2::{Compress, Compression, FlushCompress};

/// Cached fragments in a response body, in the order they appear in it: set by the controller,
/// used by the deflater when it gzips the body.
#[derive(Debug, Clone, Default)]
pub struct CachedFragments(pub Vec<Arc<String>>);

/// Smaller fragments aren't worth a separately stored piece.
const MIN_FRAGMENT: usize = 1024;
/// At most this much text between two fragments for the second to chain onto the first.
const MAX_GLUE: usize = 256;
/// Deflate's window.
const WINDOW: usize = 32 * 1024;
/// A bound on stored pieces (a 32 MB fragment cache holds a few thousand messages; a chained
/// piece is a few hundred bytes).
const MAX_PIECES: usize = 8 * 1024;

/// The whole gzip member for `body`, or `None` when no fragment is found in it (the caller then
/// streams as usual). `mtime` and the Unix OS code go in the header, as `Zlib::GzipWriter`
/// writes them.
pub fn gzip(body: &[u8], fragments: &[Arc<String>], mtime: u32) -> Option<Vec<u8>> {
    let segments = locate(body, fragments);
    if !segments.iter().any(|s| s.chained.is_some()) {
        return None;
    }

    let mut out = Vec::with_capacity(body.len() / 8);
    out.extend_from_slice(&[0x1f, 0x8b, 8, 0]);
    out.extend_from_slice(&mtime.to_le_bytes());
    out.extend_from_slice(&[0, 3]);

    let pieces = pieces(body, &segments);
    let mut live = Compress::new(Compression::default(), false);
    let mut fresh = true;
    let mut offset = 0;
    for (segment, piece) in segments.iter().zip(&pieces) {
        let Some(piece) = piece else { continue };
        // The live text before this piece, then the piece itself.
        deflate_live(&mut live, &mut fresh, body, offset, segment.glue_start, &mut out);
        out.extend_from_slice(&piece.deflated);
        offset = segment.end;
    }
    deflate_live(&mut live, &mut fresh, body, offset, body.len(), &mut out);
    // An empty final block (fixed Huffman), after the sync flushes that ended every part.
    out.extend_from_slice(&[0x03, 0x00]);
    // One pass over the whole body is cheaper than combining a CRC per piece.
    out.extend_from_slice(&crc32fast::hash(body).to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    Some(out)
}

/// A fragment found in the body, and the fragment it chains onto, if any.
struct Segment<'a> {
    fragment: &'a Arc<String>,
    /// Where the text between the previous fragment and this one starts (`start` when unchained).
    glue_start: usize,
    end: usize,
    chained: Option<&'a Arc<String>>,
}

/// Finds each fragment after the previous one. A fragment rendered but not in the body as is
/// (escaped into a turbo stream, say) is skipped.
fn locate<'a>(body: &[u8], fragments: &'a [Arc<String>]) -> Vec<Segment<'a>> {
    let mut segments: Vec<Segment<'a>> = Vec::new();
    let mut position = 0;
    for fragment in fragments.iter().filter(|f| f.len() >= MIN_FRAGMENT) {
        let Some(start) = find(body, position, fragment.as_bytes()) else { continue };
        let end = start + fragment.len();
        let chained = segments.last().filter(|previous| start - previous.end <= MAX_GLUE);
        let (glue_start, chained) = match chained {
            Some(previous) => (previous.end, Some(previous.fragment)),
            None => (start, None),
        };
        segments.push(Segment { fragment, glue_start, end, chained });
        position = end;
    }
    segments.retain(|segment| segment.chained.is_some());
    segments
}

/// The first occurrence of `needle` in `body` at or after `from`. Finding a short prefix and
/// comparing the rest is much cheaper than a whole-needle search, whose setup is linear in the
/// needle and is paid per fragment.
fn find(body: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    let prefix = &needle[..needle.len().min(64)];
    let finder = memchr::memmem::Finder::new(prefix);
    let mut at = from;
    while let Some(found) = finder.find(&body[at..]) {
        let start = at + found;
        if body[start..].starts_with(needle) {
            return Some(start);
        }
        at = start + 1;
    }
    None
}

/// A fragment's stored piece: the glue before it and the fragment, compressed against the
/// predecessor and sync-flushed (so it ends on a byte boundary with no final block).
struct Piece {
    fragment: Weak<String>,
    predecessor: Weak<String>,
    glue: Box<[u8]>,
    deflated: Bytes,
}

static PIECES: LazyLock<Mutex<HashMap<usize, Arc<Piece>>>> = LazyLock::new(Mutex::default);

/// Each segment's stored piece, compressing (and storing) the ones missing or stale.
fn pieces(body: &[u8], segments: &[Segment]) -> Vec<Option<Arc<Piece>>> {
    let mut found: Vec<Option<Arc<Piece>>> = {
        let stored = lock();
        segments.iter().map(|segment| stored.get(&key(segment.fragment)).filter(|p| p.fits(body, segment)).cloned()).collect()
    };
    let mut compressed = Vec::new();
    for (segment, piece) in segments.iter().zip(found.iter_mut()) {
        if piece.is_none() {
            let new = Arc::new(Piece::compress(body, segment));
            compressed.push((key(segment.fragment), new.clone()));
            *piece = Some(new);
        }
    }
    if !compressed.is_empty() {
        let mut stored = lock();
        if stored.len() + compressed.len() > MAX_PIECES {
            stored.retain(|_, piece| piece.fragment.strong_count() > 0);
            if stored.len() + compressed.len() > MAX_PIECES {
                stored.clear();
            }
        }
        stored.extend(compressed);
    }
    found
}

impl Piece {
    fn compress(body: &[u8], segment: &Segment) -> Self {
        let predecessor = segment.chained.expect("only chained segments get pieces");
        let start = segment.end - segment.fragment.len();
        let text = &body[segment.glue_start..segment.end];
        let dictionary = &body[segment.glue_start.saturating_sub(WINDOW).max(segment.glue_start - predecessor.len())..segment.glue_start];
        let mut deflate = Compress::new(Compression::default(), false);
        deflate.set_dictionary(dictionary).expect("a fresh raw deflate stream takes a dictionary");
        let mut deflated = Vec::with_capacity(text.len() / 4 + 64);
        compress_all(&mut deflate, text, &mut deflated);
        Self {
            fragment: Arc::downgrade(segment.fragment),
            predecessor: Arc::downgrade(predecessor),
            glue: body[segment.glue_start..start].into(),
            deflated: deflated.into(),
        }
    }

    /// Whether this piece decodes correctly at `segment`: the same predecessor right before it,
    /// with the same text between them. (A `Weak` keeps its allocation, so a live fragment at the
    /// same address is the same fragment.)
    fn fits(&self, body: &[u8], segment: &Segment) -> bool {
        let start = segment.end - segment.fragment.len();
        segment.chained.is_some_and(|predecessor| std::ptr::eq(self.predecessor.as_ptr(), Arc::as_ptr(predecessor)))
            && *self.glue == body[segment.glue_start..start]
    }
}

fn key(fragment: &Arc<String>) -> usize {
    Arc::as_ptr(fragment) as usize
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<usize, Arc<Piece>>> {
    PIECES.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Compresses `body[from..to]` on the live stream, with the output before it as the dictionary
/// when a stored piece came in between.
fn deflate_live(live: &mut Compress, fresh: &mut bool, body: &[u8], from: usize, to: usize, out: &mut Vec<u8>) {
    if from == to {
        return;
    }
    if !*fresh {
        live.reset();
        live.set_dictionary(&body[from.saturating_sub(WINDOW)..from]).expect("a reset raw deflate stream takes a dictionary");
    } else if from > 0 {
        live.set_dictionary(&body[from.saturating_sub(WINDOW)..from]).expect("a fresh raw deflate stream takes a dictionary");
    }
    *fresh = false;
    compress_all(live, &body[from..to], out);
}

/// Compresses all of `input` and sync-flushes.
fn compress_all(deflate: &mut Compress, input: &[u8], out: &mut Vec<u8>) {
    let start = deflate.total_in();
    loop {
        let consumed = (deflate.total_in() - start) as usize;
        if out.capacity() - out.len() < 1024 {
            out.reserve(out.capacity().max(4096));
        }
        deflate.compress_vec(&input[consumed..], out, FlushCompress::Sync).expect("deflate doesn't fail on valid input");
        let consumed = (deflate.total_in() - start) as usize;
        // Done once all input is in and the flush left spare room in the output.
        if consumed == input.len() && out.len() < out.capacity() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn gunzip(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes).read_to_end(&mut out).unwrap();
        out
    }

    fn message(n: usize) -> Arc<String> {
        Arc::new(format!("<div id=\"message_{n}\" class=\"message\">{}</div>\n", "<button>Boost</button> hello there ".repeat(40 + n % 7)))
    }

    fn page(head: &str, messages: &[Arc<String>], tail: &str) -> String {
        let mut page = head.to_string();
        for message in messages {
            page.push_str("  ");
            page.push_str(message);
        }
        page.push_str(tail);
        page
    }

    #[test]
    fn decodes_to_the_body_and_reuses_pieces() {
        let messages: Vec<_> = (0..30).map(message).collect();
        for token in ["abc", "a-different-token"] {
            let body = page(&format!("<html><meta name=\"csrf-token\" content=\"{token}\">"), &messages, "</html>");
            let gz = gzip(body.as_bytes(), &messages, 1234).expect("spliced");
            assert_eq!(gunzip(&gz), body.as_bytes());
            assert_eq!(&gz[4..8], &1234u32.to_le_bytes());
            assert_eq!(gz[9], 3);
        }
        let stored = lock();
        let piece = stored.get(&key(&messages[5])).unwrap().clone();
        drop(stored);
        let body = page("<html>", &messages, "</html>");
        gzip(body.as_bytes(), &messages, 0).unwrap();
        assert!(Arc::ptr_eq(&piece, lock().get(&key(&messages[5])).unwrap()), "the second page reuses the piece");
    }

    #[test]
    fn a_new_predecessor_or_glue_recompresses() {
        let messages: Vec<_> = (100..110).map(message).collect();
        let body = page("<p>", &messages, "</p>");
        gzip(body.as_bytes(), &messages, 0).unwrap();
        // Drop one message: the one after it now follows a different predecessor.
        let mut fewer = messages.clone();
        fewer.remove(4);
        let body = page("<p>", &fewer, "</p>");
        assert_eq!(gunzip(&gzip(body.as_bytes(), &fewer, 0).unwrap()), body.as_bytes());
        // Different glue between the same fragments.
        let mut body = String::from("<p>");
        for message in &messages {
            body.push_str("\n    ");
            body.push_str(message);
        }
        assert_eq!(gunzip(&gzip(body.as_bytes(), &messages, 0).unwrap()), body.as_bytes());
    }

    #[test]
    fn missing_fragments_and_small_ones_are_compressed_live() {
        let messages: Vec<_> = (200..206).map(message).collect();
        let small = Arc::new("<i>small</i>".to_string());
        let absent = message(999);
        let listed = vec![messages[0].clone(), small.clone(), absent, messages[1].clone(), messages[2].clone()];
        let body = page("<p>", &[messages[0].clone(), small, messages[1].clone(), messages[2].clone()], "</p>");
        assert_eq!(gunzip(&gzip(body.as_bytes(), &listed, 0).unwrap()), body.as_bytes());
        assert!(gzip(b"<p>nothing cached</p>", &messages, 0).is_none());
    }

    #[test]
    fn repeated_fragments_and_long_glue() {
        let a = message(300);
        let b = message(301);
        let body = format!("{a}{b}{a}{}{b}{a}", "x".repeat(MAX_GLUE + 1));
        let listed = vec![a.clone(), b.clone(), a.clone(), b.clone(), a.clone()];
        assert_eq!(gunzip(&gzip(body.as_bytes(), &listed, 0).unwrap()), body.as_bytes());
    }

    #[test]
    fn stays_close_to_whole_body_compression() {
        use std::io::Write;
        let messages: Vec<_> = (400..440).map(message).collect();
        let body = page(&"<head>layout</head>".repeat(200), &messages, &"<footer/>".repeat(300));
        let mut whole = flate2::write::GzEncoder::new(Vec::new(), Compression::default());
        whole.write_all(body.as_bytes()).unwrap();
        let whole = whole.finish().unwrap().len();
        let spliced = gzip(body.as_bytes(), &messages, 0).unwrap().len();
        // Each piece costs its flush marker and block header, not a recompressed message.
        assert!(spliced < whole + 40 * messages.len(), "{spliced} bytes spliced vs {whole} whole");
    }
}

