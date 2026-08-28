//! Integration tests for `rawterm`'s pure decoder — the part that needs no
//! terminal. Fixture sequences are verified fed one-shot and byte-at-a-time
//! (chunk boundaries can split escapes and UTF-8 anywhere), lone-ESC
//! ambiguity is resolved only by `flush`, and bracketed paste survives
//! arbitrary splits including inside its terminator.

use rawterm::{Decoder, Event, Key};

fn decode_all(input: &[u8]) -> Vec<Event> {
    let mut d = Decoder::new();
    let mut out = d.feed(input);
    out.extend(d.flush());
    out
}

fn fixtures() -> Vec<(&'static [u8], Vec<Event>)> {
    vec![
        // plain characters
        (b"a", vec![Event::key(Key::Char('a'))]),
        (b"A", vec![Event::key(Key::Char('A'))]),
        (
            b"hi",
            vec![Event::key(Key::Char('h')), Event::key(Key::Char('i'))],
        ),
        ("é".as_bytes(), vec![Event::key(Key::Char('é'))]),
        ("😀".as_bytes(), vec![Event::key(Key::Char('😀'))]),
        // controls
        (b"\r", vec![Event::key(Key::Enter)]),
        (b"\t", vec![Event::key(Key::Tab)]),
        (b"\x7f", vec![Event::key(Key::Backspace)]),
        (b"\x08", vec![Event::key(Key::Backspace).ctrl()]),
        (b"\x03", vec![Event::key(Key::Char('c')).ctrl()]),
        (b"\x1a", vec![Event::key(Key::Char('z')).ctrl()]),
        (b"\x00", vec![Event::key(Key::Char(' ')).ctrl()]),
        (b"\x1f", vec![Event::key(Key::Char('_')).ctrl()]),
        // CSI keys
        (b"\x1b[A", vec![Event::key(Key::Up)]),
        (b"\x1b[B", vec![Event::key(Key::Down)]),
        (b"\x1b[C", vec![Event::key(Key::Right)]),
        (b"\x1b[D", vec![Event::key(Key::Left)]),
        (b"\x1b[H", vec![Event::key(Key::Home)]),
        (b"\x1b[F", vec![Event::key(Key::End)]),
        (b"\x1b[Z", vec![Event::key(Key::Tab).shift()]),
        (b"\x1b[2~", vec![Event::key(Key::Insert)]),
        (b"\x1b[3~", vec![Event::key(Key::Delete)]),
        (b"\x1b[5~", vec![Event::key(Key::PageUp)]),
        (b"\x1b[6~", vec![Event::key(Key::PageDown)]),
        (b"\x1b[1~", vec![Event::key(Key::Home)]),
        (b"\x1b[8~", vec![Event::key(Key::End)]),
        // function keys, both encodings
        (b"\x1bOP", vec![Event::key(Key::F(1))]),
        (b"\x1bOS", vec![Event::key(Key::F(4))]),
        (b"\x1b[15~", vec![Event::key(Key::F(5))]),
        (b"\x1b[21~", vec![Event::key(Key::F(10))]),
        (b"\x1b[24~", vec![Event::key(Key::F(12))]),
        // modifiers (xterm ;m parameter)
        (b"\x1b[1;5C", vec![Event::key(Key::Right).ctrl()]),
        (b"\x1b[1;2A", vec![Event::key(Key::Up).shift()]),
        (b"\x1b[1;3D", vec![Event::key(Key::Left).alt()]),
        (b"\x1b[1;6B", vec![Event::key(Key::Down).ctrl().shift()]),
        (b"\x1b[3;5~", vec![Event::key(Key::Delete).ctrl()]),
        (b"\x1b[5;3~", vec![Event::key(Key::PageUp).alt()]),
        // alt + character / control
        (b"\x1bx", vec![Event::key(Key::Char('x')).alt()]),
        (b"\x1b\r", vec![Event::key(Key::Enter).alt()]),
        ("\x1bé".as_bytes(), vec![Event::key(Key::Char('é')).alt()]),
        // ESC ESC: first resolves as a key immediately
        (
            b"\x1b\x1bx",
            vec![Event::key(Key::Esc), Event::key(Key::Char('x')).alt()],
        ),
        // unknown CSI reports are consumed silently
        (b"\x1b[?1;2c", vec![]),
        (
            b"a\x1b[Bb",
            vec![
                Event::key(Key::Char('a')),
                Event::key(Key::Down),
                Event::key(Key::Char('b')),
            ],
        ),
        // bracketed paste
        (
            b"\x1b[200~hello\nworld\x1b[201~",
            vec![Event::Paste("hello\nworld".into())],
        ),
        (
            b"\x1b[200~a\x1b[Bb\x1b[201~x",
            vec![
                Event::Paste("a\x1b[Bb".into()), // sequences inside paste are payload
                Event::key(Key::Char('x')),
            ],
        ),
        // invalid bytes never desync
        (b"\xff", vec![Event::key(Key::Char('\u{FFFD}'))]),
    ]
}

#[test]
fn fixtures_one_shot() {
    for (input, expected) in fixtures() {
        assert_eq!(
            decode_all(input),
            expected,
            "input: {:?}",
            String::from_utf8_lossy(input)
        );
    }
}

#[test]
fn fixtures_split_at_every_byte() {
    for (input, expected) in fixtures() {
        let mut d = Decoder::new();
        let mut out = Vec::new();
        for &b in input {
            out.extend(d.feed(&[b]));
        }
        out.extend(d.flush());
        assert_eq!(
            out,
            expected,
            "bytewise: {:?}",
            String::from_utf8_lossy(input)
        );
    }
}

#[test]
fn lone_esc_resolves_only_on_flush() {
    let mut d = Decoder::new();
    assert_eq!(d.feed(b"\x1b"), vec![]);
    assert!(d.has_pending());
    assert_eq!(d.flush(), vec![Event::key(Key::Esc)]);
    assert!(!d.has_pending());
}

#[test]
fn esc_bracket_pending_flushes_as_esc() {
    // ESC [ with no final byte and no continuation coming: Esc, then '['.
    let mut d = Decoder::new();
    assert_eq!(d.feed(b"\x1b["), vec![]);
    assert_eq!(
        d.flush(),
        vec![Event::key(Key::Esc), Event::key(Key::Char('['))]
    );
}

#[test]
fn partial_utf8_is_held_across_feeds() {
    let mut d = Decoder::new();
    let bytes = "é".as_bytes();
    assert_eq!(d.feed(&bytes[..1]), vec![]);
    assert_eq!(d.feed(&bytes[1..]), vec![Event::key(Key::Char('é'))]);
}

#[test]
fn paste_split_inside_terminator() {
    let mut d = Decoder::new();
    let mut out = d.feed(b"\x1b[200~data\x1b[20");
    assert_eq!(out, vec![]); // partial terminator must not leak as payload
    out.extend(d.feed(b"1~"));
    assert_eq!(out, vec![Event::Paste("data".into())]);
}

#[test]
fn unterminated_paste_is_delivered_on_flush() {
    let mut d = Decoder::new();
    assert_eq!(d.feed(b"\x1b[200~oops"), vec![]);
    assert_eq!(d.flush(), vec![Event::Paste("oops".into())]);
}

#[test]
fn events_queue_across_interleaved_feeds() {
    let mut d = Decoder::new();
    let mut out = d.feed(b"a\x1b");
    out.extend(d.feed(b"[1;5Cb"));
    assert_eq!(
        out,
        vec![
            Event::key(Key::Char('a')),
            Event::key(Key::Right).ctrl(),
            Event::key(Key::Char('b')),
        ]
    );
}
