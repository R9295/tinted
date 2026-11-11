// Gap: ordering. Validation runs after the sink. The call graph has no order.
// want: FAIL
// today: OK
// invariant: validation is assumed to precede execution
use std::hint::black_box;

fn check(x: u64) {
    assert!(x < 100);
}

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

#[tinted::validate(data)]
fn process(x: u64) {
    execute(x);
    check(x);
}

fn main() {
    process(black_box(1000));
}
