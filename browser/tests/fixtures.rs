//! The parser run against the bytes the board is actually served.
//!
//! `tests/fixtures/` is written by
//! `python3 tools/browser_fixture_server.py --dump browser/tests/fixtures`,
//! so these are the same documents `/simple.html` and the rest return over
//! HTTP rather than a second copy that drifts away from them. Re-run that
//! command after changing a fixture; [`fixtures_are_current`] fails if the
//! two ever disagree in count.
//!
//! Two things are checked for every fixture, and they are the Stage 3
//! completion condition from `docs/WEB_BROWSER_PLAN.md`:
//!
//! 1. it parses into the text, blocks and links it is supposed to
//! 2. feeding it one byte at a time gives exactly the same document
//!
//! The second is the one that catches real bugs. A tokenizer that works on
//! whole documents and breaks on a boundary inside a character reference,
//! a multi-byte character or `</script` is a tokenizer that works in tests
//! and fails on the network, where nobody chooses the chunk size.

use tab5_browser::document::{BlockKind, Document, Marker, Parser, STYLE_BOLD, STYLE_CODE};
use tab5_browser::url::Url;

/// Every dumped fixture, by the name the server writes it under.
///
/// Listed rather than globbed: a test that discovers its own inputs passes
/// when they all disappear.
const FIXTURES: &[(&str, &[u8])] = &[
    ("index.html", include_bytes!("fixtures/index.html")),
    ("simple.html", include_bytes!("fixtures/simple.html")),
    ("long.html", include_bytes!("fixtures/long.html")),
    ("headings.html", include_bytes!("fixtures/headings.html")),
    ("list.html", include_bytes!("fixtures/list.html")),
    ("pre.html", include_bytes!("fixtures/pre.html")),
    ("entity.html", include_bytes!("fixtures/entity.html")),
    ("utf8.html", include_bytes!("fixtures/utf8.html")),
    ("inline.html", include_bytes!("fixtures/inline.html")),
    ("image.html", include_bytes!("fixtures/image.html")),
    ("rule.html", include_bytes!("fixtures/rule.html")),
    ("script.html", include_bytes!("fixtures/script.html")),
    ("empty.html", include_bytes!("fixtures/empty.html")),
    ("links-index.html", include_bytes!("fixtures/links-index.html")),
    ("links-target.html", include_bytes!("fixtures/links-target.html")),
    ("links-deep.html", include_bytes!("fixtures/links-deep.html")),
    (
        "broken-unclosed.html",
        include_bytes!("fixtures/broken-unclosed.html"),
    ),
    (
        "broken-deep-nest.html",
        include_bytes!("fixtures/broken-deep-nest.html"),
    ),
    (
        "broken-huge-attribute.html",
        include_bytes!("fixtures/broken-huge-attribute.html"),
    ),
    (
        "broken-bad-utf8.html",
        include_bytes!("fixtures/broken-bad-utf8.html"),
    ),
    ("slow.html", include_bytes!("fixtures/slow.html")),
    ("chunked.html", include_bytes!("fixtures/chunked.html")),
    ("limit-url.html", include_bytes!("fixtures/limit-url.html")),
    (
        "limit-longline.html",
        include_bytes!("fixtures/limit-longline.html"),
    ),
];

/// The base every fixture is parsed against, standing in for the address
/// the board would have fetched it from.
fn base() -> Url {
    Url::parse("http://fixture.local:8080/links/page.html").unwrap()
}

fn parse_in_chunks(input: &[u8], size: usize) -> Document {
    let mut parser = Parser::new(base()).unwrap();
    for chunk in input.chunks(size) {
        parser.feed(chunk).unwrap();
    }
    parser.finish().unwrap()
}

fn parse(input: &[u8]) -> Document {
    parse_in_chunks(input, input.len().max(1))
}

/// A whole document as one comparable string: one line per block, with its
/// kind, and its text with runs marked off.
fn describe(document: &Document) -> String {
    let mut description = String::new();
    description.push_str(&format!("title:{}\n", document.title()));
    for block in document.blocks() {
        let kind = match block.kind {
            BlockKind::Paragraph => "p".to_string(),
            BlockKind::Heading(level) => format!("h{level}"),
            BlockKind::ListItem { depth, marker } => match marker {
                Marker::Bullet => format!("li{depth}"),
                Marker::Number(number) => format!("li{depth}#{number}"),
            },
            BlockKind::Preformatted => "pre".to_string(),
            BlockKind::Rule => "hr".to_string(),
        };
        description.push_str(&kind);
        description.push('|');
        for run in document.block_runs(block) {
            description.push_str(&format!(
                "[{}:{}]{}",
                run.style,
                run.link.map_or(-1i32, |index| index as i32),
                document.run_text(run)
            ));
        }
        description.push('\n');
    }
    for link in document.links() {
        description.push_str(&format!("link:{}\n", link.url.to_text().unwrap()));
    }
    description
}

fn text_of(document: &Document) -> String {
    document.text().to_string()
}

// --- the two properties, for every fixture --------------------------------

#[test]
fn every_fixture_parses() {
    for (name, content) in FIXTURES {
        let mut parser = Parser::new(base()).unwrap();
        parser
            .feed(content)
            .unwrap_or_else(|error| panic!("{name}: {error:?}"));
        parser
            .finish()
            .unwrap_or_else(|error| panic!("{name}: {error:?}"));
    }
}

#[test]
fn one_byte_at_a_time_gives_the_same_document() {
    for (name, content) in FIXTURES {
        let whole = describe(&parse(content));
        let split = describe(&parse_in_chunks(content, 1));
        assert_eq!(split, whole, "{name} differs when fed one byte at a time");
    }
}

/// Several chunk sizes, including ones that are coprime with everything in
/// the documents so the boundaries walk through them.
#[test]
fn no_chunk_size_changes_the_document() {
    for (name, content) in FIXTURES {
        let whole = describe(&parse(content));
        for size in [1, 2, 3, 5, 7, 13, 31, 64, 127, 512, 4096] {
            let split = describe(&parse_in_chunks(content, size));
            assert_eq!(split, whole, "{name} differs in chunks of {size}");
        }
    }
}

/// The dumped set and the server's own list have to stay the same size.
///
/// A weak check, but the one that can be made without running Python from
/// a test: a fixture added to the server and not dumped shows up here.
#[test]
fn fixtures_are_current() {
    let listed = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures"))
        .expect("tests/fixtures is missing; run the server's --dump")
        .count();
    assert_eq!(
        listed,
        FIXTURES.len(),
        "tests/fixtures has {listed} files but {} are compiled in; re-run \
         `python3 tools/browser_fixture_server.py --dump browser/tests/fixtures` \
         and update FIXTURES",
        FIXTURES.len()
    );
}

// --- what each fixture is supposed to become ------------------------------

#[test]
fn simple_has_a_title_a_heading_and_a_link() {
    let document = parse(FIXTURES[1].1);
    assert_eq!(document.title(), "simple");
    assert_eq!(document.blocks().len(), 3);
    assert_eq!(document.blocks()[0].kind, BlockKind::Heading(1));
    assert_eq!(document.links().len(), 1);
    assert_eq!(
        document.links()[0].url.to_text().unwrap(),
        "http://fixture.local:8080/links/target.html"
    );
    assert!(text_of(&document).contains("This page has a title"));
}

#[test]
fn headings_keep_all_six_levels() {
    let document = parse(include_bytes!("fixtures/headings.html"));
    let levels: Vec<u8> = document
        .blocks()
        .iter()
        .filter_map(|block| match block.kind {
            BlockKind::Heading(level) => Some(level),
            _ => None,
        })
        .collect();
    assert_eq!(levels, [1, 2, 3, 4, 5, 6]);
}

#[test]
fn lists_nest_and_number() {
    let document = parse(include_bytes!("fixtures/list.html"));
    let items: Vec<(u8, Marker)> = document
        .blocks()
        .iter()
        .filter_map(|block| match block.kind {
            BlockKind::ListItem { depth, marker } => Some((depth, marker)),
            _ => None,
        })
        .collect();
    assert_eq!(
        items,
        [
            (0, Marker::Bullet),
            (0, Marker::Bullet),
            (1, Marker::Bullet),
            (1, Marker::Bullet),
            (2, Marker::Number(1)),
            (2, Marker::Number(2)),
            (0, Marker::Bullet),
            (0, Marker::Number(1)),
            (0, Marker::Number(2)),
            (0, Marker::Number(3)),
        ]
    );
}

#[test]
fn pre_keeps_its_columns_and_the_paragraph_after_it_does_not() {
    let document = parse(include_bytes!("fixtures/pre.html"));
    let pre = document
        .blocks()
        .iter()
        .find(|block| block.kind == BlockKind::Preformatted)
        .expect("no pre block");
    let mut text = String::new();
    for run in document.block_runs(pre) {
        text.push_str(document.run_text(run));
    }
    assert!(text.contains("  alpha      1      2"), "{text}");
    assert!(text.contains('\n'), "{text}");
    // The paragraph after it collapses again.
    assert!(document.text().contains("collapses its whitespace"));
}

#[test]
fn entities_resolve_and_unresolvable_ones_survive() {
    let document = parse(include_bytes!("fixtures/entity.html"));
    let text = text_of(&document);
    assert!(text.contains("& < > \" ' \u{00A0}end"), "{text}");
    assert!(text.contains("A — \u{1F4A9}"), "{text}");
    // Written as typed, because nothing was lost.
    assert!(text.contains("&notareference;"), "{text}");
    assert!(text.contains("&#;"), "{text}");
    // `&amp;` inside an attribute became a real ampersand in the target.
    assert_eq!(
        document.links()[0].url.to_text().unwrap(),
        "http://fixture.local:8080/links/target.html?a=1&b=2"
    );
}

#[test]
fn non_ascii_text_survives_intact() {
    let document = parse(include_bytes!("fixtures/utf8.html"));
    let text = text_of(&document);
    for expected in ["é ü ñ αβγ", "日本語 ひらがな カタカナ", "\u{1F600}"] {
        assert!(text.contains(expected), "{expected} missing from {text}");
    }
}

#[test]
fn inline_elements_become_styles_rather_than_text() {
    let document = parse(include_bytes!("fixtures/inline.html"));
    let styled: Vec<(u8, &str)> = document
        .runs()
        .iter()
        .filter(|run| run.style != 0)
        .map(|run| (run.style, document.run_text(run)))
        .collect();
    assert!(styled.iter().any(|(style, text)| *style & STYLE_BOLD != 0
        && *text == "strong"));
    assert!(styled.iter().any(|(style, text)| *style & STYLE_CODE != 0
        && text.contains("code()")));
    // The markup inside `<code>` came through as text, not as tags.
    assert!(document.text().contains("<tag attribute=\"value\">"));
    assert!(!document.text().contains("<strong>"));
}

#[test]
fn images_become_bracketed_alt_text() {
    let document = parse(include_bytes!("fixtures/image.html"));
    let text = text_of(&document);
    assert!(text.contains("[a red square]"), "{text}");
    assert!(text.contains("[image]"), "{text}");
    assert!(text.contains("[self closed]"), "{text}");
    assert!(!text.contains("missing.png"), "{text}");
}

#[test]
fn rules_become_their_own_blocks() {
    let document = parse(include_bytes!("fixtures/rule.html"));
    let rules = document
        .blocks()
        .iter()
        .filter(|block| block.kind == BlockKind::Rule)
        .count();
    assert_eq!(rules, 2);
}

#[test]
fn script_and_style_bodies_are_not_displayed() {
    let document = parse(include_bytes!("fixtures/script.html"));
    let text = text_of(&document);
    assert!(text.contains("Between the style and the script"), "{text}");
    assert!(text.contains("After the script"), "{text}");
    for forbidden in [
        "content:",
        "document.write",
        "</style is not the end",
        "</script is only ended",
        "var s",
    ] {
        assert!(!text.contains(forbidden), "{forbidden} leaked into {text}");
    }
}

#[test]
fn an_empty_document_has_nothing_in_it() {
    let document = parse(include_bytes!("fixtures/empty.html"));
    assert_eq!(document.title(), "empty");
    assert!(document.blocks().is_empty());
    assert_eq!(document.text(), "");
    assert!(document.links().is_empty());
}

#[test]
fn the_link_index_resolves_every_shape_of_reference() {
    let document = parse(include_bytes!("fixtures/links-index.html"));
    let targets: Vec<String> = document
        .links()
        .iter()
        .map(|link| link.url.to_text().unwrap())
        .collect();
    // The base is `http://fixture.local:8080/links/page.html`, so a
    // same-directory reference stays under `/links/`.
    assert!(targets.contains(&"http://fixture.local:8080/links/target.html".to_string()));
    assert!(targets.contains(&"http://fixture.local:8080/links/deep/target.html".to_string()));
    assert!(targets.contains(&"http://fixture.local:8080/simple.html".to_string()));
    assert!(targets.contains(&"http://example.invalid/absolute".to_string()));
    assert!(targets.contains(&"https://example.invalid/secure".to_string()));
    // `ftp://` is not a link this can offer, and its text stays.
    assert!(!targets.iter().any(|target| target.starts_with("ftp:")));
    assert!(document.text().contains("unsupported scheme"));
}

#[test]
fn unterminated_markup_keeps_every_word() {
    let document = parse(include_bytes!("fixtures/broken-unclosed.html"));
    let text = text_of(&document);
    for expected in [
        "Unterminated markup",
        "A paragraph that is never closed.",
        "one",
        "two",
        "three",
        "unclosed link",
        "and some unclosed containers",
    ] {
        assert!(text.contains(expected), "{expected} missing from {text}");
    }
    assert_eq!(document.links().len(), 1);
}

#[test]
fn nesting_far_past_the_bound_still_reaches_the_innermost_item() {
    let document = parse(include_bytes!("fixtures/broken-deep-nest.html"));
    assert!(
        document
            .text()
            .contains("the innermost item, well past MAX_NESTING_DEPTH")
    );
}

#[test]
fn an_element_buried_in_attributes_still_yields_its_text() {
    let document = parse(include_bytes!("fixtures/broken-huge-attribute.html"));
    let text = text_of(&document);
    assert!(text.contains("Text under an element carrying more attributes"));
    assert!(text.contains("Second paragraph, to show the parser recovered."));
    // None of the attribute values became content.
    assert!(!text.contains("vvvvvvvv"), "attribute values leaked");
    assert!(!text.contains("tttttttt"), "attribute values leaked");
}

#[test]
fn broken_encoding_costs_one_character_and_no_more() {
    let document = parse(include_bytes!("fixtures/broken-bad-utf8.html"));
    let text = text_of(&document);
    assert!(text.contains("before"), "{text}");
    assert!(text.contains("after"), "{text}");
    assert!(text.contains("done"), "{text}");
    assert!(text.contains("日本語"), "{text}");
    assert!(text.contains('\u{FFFD}'), "{text}");
}

#[test]
fn an_over_long_link_is_dropped_and_the_next_one_is_not() {
    let document = parse(include_bytes!("fixtures/limit-url.html"));
    assert_eq!(document.links().len(), 1);
    assert_eq!(
        document.links()[0].url.to_text().unwrap(),
        "http://fixture.local:8080/simple.html"
    );
    // The over-long link's text is still on the page.
    assert!(document.text().contains("an over-long target"));
}

#[test]
fn one_unbreakable_line_is_still_one_block() {
    let document = parse(include_bytes!("fixtures/limit-longline.html"));
    assert_eq!(document.blocks().len(), 2);
    assert!(document.text().contains("A normal paragraph after it."));
}

// --- memory ---------------------------------------------------------------

#[test]
fn no_fixture_holds_more_than_its_own_text() {
    for (name, content) in FIXTURES {
        let document = parse(content);
        let stats = document.stats();
        // Everything the document owns, against the text it holds. Twice
        // the text plus a fixed allowance: the buffer grows geometrically
        // at these sizes, so up to half of it is capacity that has not been
        // used yet. What this rules out is the document holding the markup
        // as well -- every fixture's HTML is far more than twice its text.
        let budget = stats.text_bytes * 2 + stats.links * 512 + 8192;
        assert!(
            stats.owned_bytes <= budget,
            "{name}: owns {} bytes for {} of text and {} links",
            stats.owned_bytes,
            stats.text_bytes,
            stats.links
        );
    }
}

#[test]
fn the_parser_never_holds_the_whole_input() {
    let long = include_bytes!("fixtures/long.html");
    let mut parser = Parser::new(base()).unwrap();
    let mut peak_overhead = 0usize;
    for chunk in long.chunks(64) {
        parser.feed(chunk).unwrap();
        let document_text = parser.items();
        let _ = document_text;
        peak_overhead = peak_overhead.max(parser.owned_bytes());
    }
    let document = parser.finish().unwrap();
    // The parse never cost much more than the finished document does.
    assert!(
        peak_overhead < document.stats().owned_bytes + 64 * 1024,
        "peak {peak_overhead} against {} owned",
        document.stats().owned_bytes
    );
}


// --- the reference table the device's `bt` walk is diffed against ---------

/// CRC-32, the zlib/PNG polynomial.
///
/// A second implementation of one that already exists in the firmware
/// (`net::tftp::Crc32`), which is normally the wrong thing to do -- so it
/// checks itself against the polynomial's published test vector below.
/// Sharing the firmware's copy would mean this test crate depending on the
/// firmware crate, which is exactly the dependency the split exists to
/// avoid.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[test]
fn the_checksum_matches_its_published_test_vector() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
}

/// Prints the numbers the device's `bt` walk reports, for the same
/// fixtures.
///
/// Ignored by default because it asserts nothing: it exists to be run by
/// hand when the device's output needs checking against a host that runs
/// the identical parser.
///
/// ```sh
/// cargo test -p tab5-browser --target x86_64-unknown-linux-gnu \
///     --test fixtures -- --ignored --nocapture reference_metrics
/// ```
///
/// The CRC is over the document's extracted text, not over the HTML, so it
/// does not depend on the address the page was fetched from -- which is why
/// a host run and a device run are comparable at all. `links` is likewise
/// a count and not a set of targets; the targets do depend on the base, and
/// they are checked by the per-fixture tests above.
#[test]
#[ignore = "prints the reference table; asserts nothing"]
fn reference_metrics() {
    println!(
        "{:<28} {:>10} {:>7} {:>6} {:>6} {:>8}",
        "fixture", "crc32", "blocks", "runs", "links", "text"
    );
    for (name, content) in FIXTURES {
        let document = parse(content);
        let stats = document.stats();
        println!(
            "{:<28} {:>10} {:>7} {:>6} {:>6} {:>8}",
            name,
            format!("{:08X}", crc32(document.text().as_bytes())),
            document.blocks().len(),
            document.runs().len(),
            document.links().len(),
            stats.text_bytes,
        );
    }
}
