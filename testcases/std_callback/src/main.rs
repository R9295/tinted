// Gap: callbacks from code that is not in the IR. The closure is run by the
// prebuilt standard library (`Once`), so it has no caller in the graph even
// though it only ever runs under the validate function.
// want: OK
// today: FAIL
use std::sync::OnceLock;

static VALUE: OnceLock<u64> = OnceLock::new();

#[tinted::sink(data)]
fn execute(x: u64) -> u64 {
    println!("{x}");
    x
}

#[tinted::validate(data)]
fn process(x: u64) -> u64 {
    *VALUE.get_or_init(|| execute(x))
}

fn main() {
    process(7);
}
