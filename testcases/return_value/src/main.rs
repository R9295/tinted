// Coverage: a validator that returns its result in registers. The value is
// small, so no stack slot is involved; this is the shape that already works.
// want: OK
// today: OK
use std::hint::black_box;

#[tinted::validate(data)]
fn validate(x: u64) -> Option<u64> {
    (x < 1_000).then_some(x)
}

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

fn handle(x: u64) {
    if let Some(checked) = validate(x) {
        execute(checked);
    }
}

fn main() {
    handle(black_box(7));
}
