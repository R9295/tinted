// Coverage: hashing callbacks from the prebuilt standard library. Dangerous's Hash
// impl reaches the sink and is only used under validation. When the table
// grows, the standard library's hash table code calls the hasher back through
// a closure, and that code is not in the call graph, so the closure has no
// caller.
// want: OK
// today: OK
use std::{
    collections::HashSet,
    hash::{Hash, Hasher},
    hint::black_box,
};

#[derive(PartialEq, Eq)]
struct Dangerous(u64);

impl Hash for Dangerous {
    fn hash<H: Hasher>(&self, state: &mut H) {
        execute(self.0);
        self.0.hash(state);
    }
}

#[tinted::sink(data)]
fn execute(x: u64) {
    black_box(x);
}

#[tinted::validate(data)]
fn checked() {
    let mut set = HashSet::new();
    for x in 0..64 {
        set.insert(Dangerous(x));
    }
    black_box(set);
}

fn unchecked() {
    let mut set = HashSet::new();
    for x in 0..64u64 {
        set.insert(x);
    }
    black_box(set);
}

fn main() {
    checked();
    unchecked();
}
