// Coverage: the same shape as `return_slot`, with the sink as a method. The
// block comes from the validator, but the receiver (`&mut self`) does not, so
// the sink must say which argument has to be validated data.
// want: OK
// today: OK
use std::hint::black_box;

pub struct Block {
    number: u64,
    parent: u64,
    state_root: u64,
    gas_used: u64,
}

#[derive(Default)]
struct Tree {
    blocks: Vec<Block>,
}

#[tinted::validate(block)]
fn validate(raw: [u64; 4]) -> Result<Block, String> {
    if raw[3] > 30_000_000 {
        return Err("gas limit exceeded".into());
    }
    Ok(Block { number: raw[0], parent: raw[1], state_root: raw[2], gas_used: raw[3] })
}

impl Tree {
    #[tinted::sink(block)]
    fn insert(&mut self, block: Block) {
        self.blocks.push(block);
    }

    fn handle(&mut self, raw: [u64; 4]) -> Result<(), String> {
        let block = validate(raw)?;
        self.insert(block);
        Ok(())
    }
}

fn main() {
    let mut tree = Tree::default();
    tree.handle(black_box([1, 0, 7, 21_000])).unwrap();
    println!("{}", tree.blocks.len());
}
