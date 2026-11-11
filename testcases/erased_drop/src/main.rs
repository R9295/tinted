// Coverage: type-erased destructors. Only Dangerous's destructor reaches the sink,
// and it is only dropped under validation. Matching drop calls by type mixes
// the destructors together and adds callers from unused generated drop glue.
// want: OK
// today: OK
use std::{any::Any, hint::black_box};

struct Dangerous(u64);
struct Harmless(u64);

impl Drop for Dangerous {
    fn drop(&mut self) {
        execute(self.0);
    }
}

impl Drop for Harmless {
    fn drop(&mut self) {
        black_box(self.0);
    }
}

#[tinted::sink(data)]
fn execute(x: u64) {
    black_box(x);
}

#[tinted::validate(data)]
fn checked(x: Box<dyn Any>) {
    drop(x);
}

fn unchecked(x: Box<dyn Any>) {
    drop(x);
}

fn main() {
    checked(black_box(Box::new(Dangerous(7))));
    unchecked(black_box(Box::new(Harmless(7))));
}
