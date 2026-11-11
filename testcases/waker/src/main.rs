// Coverage: wakers are matched by type. A waker is a hand-written table of
// `fn(*const ())` pointers. Only the dangerous waker reaches the sink, and it
// is only woken under validation, but waking the harmless one is also
// connected to it.
// want: OK
// today: OK
use std::{
    hint::black_box,
    ptr,
    task::{RawWaker, RawWakerVTable, Waker},
};

#[tinted::sink(data)]
fn execute(x: u64) {
    black_box(x);
}

fn nothing(_: *const ()) {}

fn dangerous_wake(_: *const ()) {
    execute(7);
}

fn dangerous_clone(_: *const ()) -> RawWaker {
    RawWaker::new(ptr::null(), &DANGEROUS)
}

fn harmless_clone(_: *const ()) -> RawWaker {
    RawWaker::new(ptr::null(), &HARMLESS)
}

static DANGEROUS: RawWakerVTable =
    RawWakerVTable::new(dangerous_clone, dangerous_wake, nothing, nothing);
static HARMLESS: RawWakerVTable = RawWakerVTable::new(harmless_clone, nothing, nothing, nothing);

#[tinted::validate(data)]
fn checked(waker: Waker) {
    waker.wake();
}

fn unchecked(waker: Waker) {
    waker.wake();
}

fn main() {
    checked(black_box(unsafe { Waker::from_raw(dangerous_clone(ptr::null())) }));
    unchecked(black_box(unsafe { Waker::from_raw(harmless_clone(ptr::null())) }));
}
