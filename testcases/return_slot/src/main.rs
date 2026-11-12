// Coverage: a validator that returns its result through memory. `validate`
// returns a `Result` too large for registers, so the caller gives it a stack
// slot to write into and reads the block back out of that slot. The sink's
// argument is the validator's result, but it is called next to the
// validator, not from inside it.
// want: OK
// today: OK
use std::hint::black_box;

pub struct Block {
    number: u64,
    parent: u64,
    state_root: u64,
    gas_used: u64,
}

#[tinted::validate(block)]
fn validate(raw: [u64; 4]) -> Result<Block, String> {
    if raw[3] > 30_000_000 {
        return Err("gas limit exceeded".into());
    }
    Ok(Block { number: raw[0], parent: raw[1], state_root: raw[2], gas_used: raw[3] })
}

#[tinted::sink(block)]
fn insert(block: Block) {
    println!("{} {} {} {}", block.number, block.parent, block.state_root, block.gas_used);
}

fn handle(raw: [u64; 4]) -> Result<(), String> {
    let block = validate(raw)?;
    insert(block);
    Ok(())
}

fn main() {
    handle(black_box([1, 0, 7, 21_000])).unwrap();
}
