//! Pseudo-fuzzing for the lexer + parser (without cargo-fuzz): random
//! inputs may produce errors, but must never panic or hang.
//! Deterministically seeded; TINOX_FUZZ_SEED overrides.

use tinox_lexer::Lexer;
use tinox_parser::Parser;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn seed() -> u64 {
    std::env::var("TINOX_FUZZ_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x7451_0710_2026_0002)
}

fn lex_and_parse(src: &str) {
    let mut lexer = Lexer::new(src);
    if let Ok(tokens) = lexer.tokenize() {
        let mut parser = Parser::new(tokens);
        let _ = parser.parse();
    }
}

/// Watchdog: a hanging input should make the test fail loudly and
/// quickly, not block the suite for hours. (The hanging thread leaks —
/// irrelevant for a failing test run.)
fn check_no_hang(src: &str) {
    use std::sync::mpsc::channel;
    use std::time::Duration;
    let (tx, rx) = channel();
    let owned = src.to_string();
    std::thread::spawn(move || {
        lex_and_parse(&owned);
        let _ = tx.send(());
    });
    if rx.recv_timeout(Duration::from_secs(10)).is_err() {
        panic!("Lexer/parser hangs on input ({} chars):\n{src}", src.len());
    }
}

/// A valid program that touches on every language construct — the basis for mutations.
const VALID_BASE: &str = r#"
import tinox.core.json

enum Shape { Circle(Float64)
Rect(Float64, Float64)
Dot }

interface Area { fn area() -> Float64 }

class Box implements Area {
    var w: Float64
    var items: List<List<String>>
    fn area() -> Float64 { return this.w * 2.0 }
}

fn describe(s: Shape) -> String {
    match s {
        Circle(r) => return "c" + r.toString()
        Rect(w, h) => { return "r" }
        _ => return "d"
    }
}

fn main() -> Int32 {
    var xs: List<Int64> = [1, 2, 3,]
    xs.push(4)
    let m: Map<String, Int64> = Map::new()
    for x in xs { println(x) }
    let f = y => y * 2
    try { throw "e" } catch e: String { println(e) } finally { println("f") }
    let s = "a\nb#\"quoted\""
    println(f(21))
    return 0
}
"#;

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(seed());
    for _ in 0..1500 {
        let len = rng.below(300);
        let s: String = (0..len)
            .map(|_| {
                // A mix of ASCII (common) and arbitrary Unicode characters (rare)
                if rng.below(10) < 9 {
                    (rng.below(95) as u8 + 32) as char
                } else {
                    char::from_u32(rng.next() as u32 % 0x10FFFF).unwrap_or('?')
                }
            })
            .collect();
        check_no_hang(&s);
    }
}

#[test]
fn mutated_programs_never_panic() {
    let mut rng = Rng(seed() ^ 0xDEAD);
    let base: Vec<char> = VALID_BASE.chars().collect();
    for _ in 0..1500 {
        let mut prog = base.clone();
        // 1–8 random mutations: delete, duplicate, replace
        for _ in 0..rng.below(8) + 1 {
            if prog.is_empty() {
                break;
            }
            let pos = rng.below(prog.len() as u64) as usize;
            match rng.below(3) {
                0 => {
                    prog.remove(pos);
                }
                1 => {
                    let c = prog[pos];
                    prog.insert(pos, c);
                }
                _ => {
                    const NASTY: &[char] =
                        &['"', '#', '\\', '{', '}', '(', ')', '[', ']', '<', '>', ';', '\n', '\0', 'ß'];
                    prog[pos] = NASTY[rng.below(NASTY.len() as u64) as usize];
                }
            }
        }
        let s: String = prog.into_iter().collect();
        check_no_hang(&s);
    }
}

#[test]
fn truncated_programs_never_panic() {
    // Every prefix of the valid program (EOF in the middle of every construct)
    for end in 0..VALID_BASE.len() {
        if VALID_BASE.is_char_boundary(end) {
            check_no_hang(&VALID_BASE[..end]);
        }
    }
}
