// Coverage: the validator is called once before a loop that calls the sink.
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
    for i in 0..black_box(3) {
        execute(x + i);
    }
}

fn main() {
    handle(black_box(7));
}
