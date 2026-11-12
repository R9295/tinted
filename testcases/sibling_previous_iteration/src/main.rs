// Negative: inside a loop the sink is called before the validator, so the
// first iteration runs unvalidated.
// want: FAIL
// today: FAIL
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
    for i in 0..black_box(3) {
        execute(x + i);
        validate(x + i);
    }
}

fn main() {
    handle(black_box(7));
}
