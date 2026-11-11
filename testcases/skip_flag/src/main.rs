// Gap: conditions. The validate function is on the chain, but a flag makes it
// skip the check. The call graph has no notion of which branch runs.
// want: FAIL
// today: OK
use std::hint::black_box;

fn check(x: u64) {
    assert!(x < 100);
}

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

#[tinted::validate(data)]
fn process(x: u64, skip_check: bool) {
    if !skip_check {
        check(x);
    }
    execute(x);
}

fn main() {
    process(black_box(1000), black_box(true));
}
