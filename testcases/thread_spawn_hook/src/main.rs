// Coverage: spawn hooks have callbacks in both the parent and child thread.
// Prebuilt std invokes them separately from the thread entry point that the
// resolver pairs with spawn_unchecked.
// want: OK
// today: OK
#![feature(thread_spawn_hook)]
use std::thread;

#[tinted::sink(data)]
fn execute(x: u64) {
    println!("{x}");
}

#[tinted::validate(data)]
fn process(x: u64) {
    thread::add_spawn_hook(move |_| {
        execute(x);
        move || execute(x)
    });
    thread::spawn(|| {}).join().unwrap();
}

fn main() {
    process(7);
}
