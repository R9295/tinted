// Coverage: the validator is called next to the sink, not above it. Every
// path to the sink first calls the validator.
// want: OK
// today: OK
use std::hint::black_box;

#[tinted::validate(data)]
fn validate(x: u64) -> bool {
    black_box(x) < 1_000
}

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

fn handle(x: u64) {
    if !validate(x) {
        return;
    }
    execute(x);
}

fn main() {
    handle(black_box(7));
}
