// Negative: the same shape as `return_slot`, but a second caller builds the
// block itself and never calls the validator.
// want: FAIL
// today: FAIL
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

fn import(raw: [u64; 4]) {
    insert(Block { number: raw[0], parent: raw[1], state_root: raw[2], gas_used: raw[3] });
}

fn main() {
    handle(black_box([1, 0, 7, 21_000])).unwrap();
    import(black_box([2, 1, 9, 99_000_000]));
}
