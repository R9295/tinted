// Coverage: one dispatch function, where every arm that calls the sink first
// calls the validator.
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

enum Event {
    Network(u64),
    Timer(u64),
    Quit,
}

fn update(event: Event) -> bool {
    match event {
        Event::Network(x) => {
            if validate(x) {
                execute(x);
            }
        }
        Event::Timer(x) => {
            if validate(x) {
                execute(x + 1);
            }
        }
        Event::Quit => return false,
    }
    true
}

fn main() {
    let events = black_box([Event::Network(7), Event::Timer(8), Event::Quit]);
    for event in events {
        if !update(event) {
            break;
        }
    }
}
