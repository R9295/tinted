// Negative: the sink is called before the validator.
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
    execute(x);
    validate(x);
}

fn main() {
    handle(black_box(7));
}
