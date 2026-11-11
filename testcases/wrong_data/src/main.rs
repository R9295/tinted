// Gap: data flow. The validate function checks one value and executes
// another. Only calls are tracked, not which data reaches the sink.
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
fn process(checked: u64, unchecked: u64) {
    check(checked);
    execute(unchecked);
}

fn main() {
    process(black_box(1), black_box(1000));
}
