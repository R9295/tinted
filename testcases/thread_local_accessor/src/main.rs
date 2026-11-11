// Coverage: thread-local accessors are matched by type. Only DANGEROUS reaches the
// sink when it is first read, and it is only read under validation, but every
// thread local of the same type has the same accessor signature, so reading
// HARMLESS is also connected to it.
// want: OK
// today: OK
use std::hint::black_box;

thread_local! {
    static DANGEROUS: u64 = execute(7);
    static HARMLESS: u64 = black_box(1);
}

#[tinted::sink(data)]
fn execute(x: u64) -> u64 {
    black_box(x)
}

#[tinted::validate(data)]
fn checked() {
    DANGEROUS.with(|value| black_box(*value));
}

fn unchecked() {
    HARMLESS.with(|value| black_box(*value));
}

fn main() {
    checked();
    unchecked();
}
