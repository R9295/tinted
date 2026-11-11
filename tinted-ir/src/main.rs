//! Checks `#[tinted::validate(id)]` / `#[tinted::sink(id)]` annotations
//! against a linked LLVM module, starting at the sinks.
//!
//! A sink is checked when every way it can run is validated:
//!
//! - Callers. Every direct caller must be a validate function, be code
//!   inlined from one, or itself only run validated (the walk continues up).
//! - Pointers. Where the address of a function on the chain is taken (as a
//!   function pointer, in a vtable, in a callback record), either the code
//!   taking the address must be validated, or every call made through that
//!   pointer must be. The second is found by following the pointer forward
//!   from where it is taken, through locals, arguments, returns and fields.
//!   A pointer handed to code outside the module is called where it is handed
//!   over. A pointer that cannot be followed is taken to be called anywhere.
//! - Data. A call of a sink is also checked when all of its arguments were
//!   produced by validated code. Arguments are traced back through locals,
//!   calls and channel receives; a receive is checked when every send of that
//!   message type is validated.
//!
//! Only code reachable from `main` counts. Validators are trusted: that they
//! check the right value, on every branch, before the sink is not verified.
//!
//! The module must be built by rustc with debug info (inlined validators and
//! channel operations are recognised from it); nothing is run or optimized.

mod ir;

use {
    ir::{ANY, Ir, V},
    llvm_sys::{LLVMOpcode, LLVMValueKind},
    std::{
        collections::{BTreeMap, HashMap, HashSet, VecDeque},
        process::ExitCode,
        rc::Rc,
    },
};

/// Byte offsets to follow from a value to reach the tracked function: the
/// value plus the first offset is the address of a pointer, and so on. Empty
/// for the function pointer itself. For an aggregate value the first offset
/// is that of the field holding the pointer.
type Path = Vec<i64>;
const MAX_PATH: usize = 6;
const MAX_FACTS: usize = 200_000;
const MAX_DATA_NODES: usize = 200_000;

fn add(a: i64, b: i64) -> i64 {
    if a == ANY || b == ANY { ANY } else { a.wrapping_add(b) }
}
fn shift(mut path: Path, delta: i64) -> Path {
    if let Some(first) = path.first_mut() {
        *first = add(*first, delta);
    }
    path
}
fn prepend(offset: i64, path: &[i64]) -> Path {
    let mut result = vec![offset];
    result.extend_from_slice(path);
    result
}

/// A place where the address of `target` is taken: operand `op` of `inst`.
struct Site {
    target: V,
    inst: V,
    op: u32,
    path: Path,
}

/// How a function is used by live code.
#[derive(Default)]
struct Uses {
    /// Direct calls of it.
    calls: Vec<V>,
    /// Places its address is taken.
    sites: Vec<u32>,
    /// Listed as a static constructor.
    constructor: bool,
}

/// What reaches the callers of a function.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Cont {
    /// The call's result holds the pointer.
    Ret(Path),
    /// The memory an argument points at holds the pointer.
    Arg(u32, Path),
}

/// Work left to do for a flow. Queued rather than done at once, so that long
/// chains of callers do not nest.
enum Item {
    /// A value holds the pointer.
    Fact(V, Path),
    /// An operand of an instruction holds the pointer.
    Use(V, u32, Path),
    /// Something reaches the callers of a function.
    Callers(V, Cont),
}

/// Where the pointer taken at one site goes.
struct Flow {
    target: V,
    seen: HashSet<(V, Path)>,
    /// Calls through the pointer, and calls handing it to outside code (true).
    uses: Vec<(V, bool)>,
    used: HashSet<V>,
    escaped: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum Node {
    /// Every run of the function is validated.
    Fn(V),
    /// The address taken at a site is only called validated.
    Creation(u32),
    /// Every call through the pointer taken at a site is validated.
    Flow(u32),
    /// A call of a sink: the caller, or all the arguments.
    SinkCall(V),
    /// All arguments of a call are validated data.
    Data(V),
    /// A value is validated data.
    Val(V),
    /// What a value is computed from is validated data.
    Operands(V),
    /// What is stored behind a pointer is validated data.
    Mem(V),
    /// An argument at a call: the caller, or the value.
    ArgAt(V, u32),
    /// The memory behind an argument at a call: the caller, or the contents.
    ArgMem(V, u32),
    /// Every send on a channel is validated.
    Sends(String),
    Yes,
    No(Rc<str>),
}

enum Expr {
    True,
    False(Rc<str>),
    All(Vec<Node>),
    Any(Vec<Node>),
    /// Not worked out yet; counts as false until it is.
    Pending,
}

struct Analysis<'a> {
    ir: &'a Ir,
    trace: bool,
    entry: Option<V>,
    live: HashSet<V>,
    // Shared by all identifiers.
    uses: HashMap<V, Rc<Uses>>,
    sites: Vec<Site>,
    site_ids: HashMap<(V, V, u32, Path), u32>,
    flows: HashMap<u32, Flow>,
    work: VecDeque<(u32, Item)>,
    asked: HashSet<(u32, V, Cont)>,
    subs: HashMap<V, Vec<(u32, Cont)>>,
    subscribed: HashSet<(V, u32, Cont)>,
    returns: HashMap<V, Rc<Vec<V>>>,
    sends: Option<HashMap<String, HashSet<V>>>,
    // Per identifier.
    validators: HashSet<V>,
    inlined_validators: HashSet<ir::Md>,
    sinks: HashSet<V>,
    graph: HashMap<Node, Expr>,
    data_nodes: usize,
    /// Functions, and caller/callee pairs, listed as known false positives.
    known_functions: HashSet<String>,
    known_calls: HashSet<(String, String)>,
    ignored: usize,
}

impl<'a> Analysis<'a> {
    fn new(ir: &'a Ir, trace: bool) -> Self {
        let entry = ir.function("main").filter(|&f| !ir.is_declaration(f));
        let mut analysis = Self {
            ir,
            trace,
            entry,
            live: HashSet::new(),
            uses: HashMap::new(),
            sites: vec![],
            site_ids: HashMap::new(),
            flows: HashMap::new(),
            work: VecDeque::new(),
            asked: HashSet::new(),
            subs: HashMap::new(),
            subscribed: HashSet::new(),
            returns: HashMap::new(),
            sends: None,
            validators: HashSet::new(),
            inlined_validators: HashSet::new(),
            sinks: HashSet::new(),
            graph: HashMap::new(),
            data_nodes: 0,
            known_functions: HashSet::new(),
            known_calls: HashSet::new(),
            ignored: 0,
        };
        analysis.mark_live();
        analysis
    }

    /// Functions reachable from `main` by any reference: a call, or an
    /// address taken directly or through a global.
    fn mark_live(&mut self) {
        let ir = self.ir;
        let Some(entry) = self.entry else {
            self.live = ir.functions().into_iter().collect();
            return;
        };
        let mut seen = HashSet::new();
        let mut pending = vec![entry];
        seen.insert(entry);
        while let Some(value) = pending.pop() {
            let mut refer = |operand: V, pending: &mut Vec<V>| {
                use LLVMValueKind::*;
                if operand.is_null() {
                    return;
                }
                match ir.kind(operand) {
                    LLVMFunctionValueKind | LLVMGlobalVariableValueKind | LLVMGlobalAliasValueKind
                    | LLVMGlobalIFuncValueKind | LLVMConstantExprValueKind | LLVMConstantArrayValueKind
                    | LLVMConstantStructValueKind | LLVMConstantVectorValueKind | LLVMConstantPtrAuthValueKind => {
                        if seen.insert(operand) {
                            pending.push(operand);
                        }
                    }
                    _ => {}
                }
            };
            if ir.is_function(value) {
                self.live.insert(value);
                for inst in ir.instructions(value) {
                    for operand in ir.operands(inst) {
                        refer(operand, &mut pending);
                    }
                }
            } else {
                for operand in ir.operands(value) {
                    refer(operand, &mut pending);
                }
            }
        }
    }

    fn validated_inst(&self, inst: V) -> bool {
        if self.inlined_validators.is_empty() {
            return false;
        }
        let mut location = self.ir.location(inst);
        while !location.is_null() {
            let at = self.ir.inlined_at(location);
            if !at.is_null() && self.inlined_validators.contains(&at) {
                return true;
            }
            location = at;
        }
        false
    }

    /// Follows a constant up through the constants and globals that contain
    /// it, to the instructions that use them.
    fn constant_uses(
        &self,
        constant: V,
        path: Path,
        out: &mut Vec<(V, u32, Path)>,
        constructor: &mut bool,
        seen: &mut HashSet<(V, Path)>,
    ) {
        use {LLVMOpcode::*, LLVMValueKind::*};
        let ir = self.ir;
        if path.len() > MAX_PATH || !seen.insert((constant, path.clone())) {
            return;
        }
        for user in ir.users(constant) {
            match ir.kind(user) {
                LLVMInstructionValueKind => {
                    if !self.live.contains(&ir.parent(user)) {
                        continue;
                    }
                    for (j, operand) in ir.operands(user).into_iter().enumerate() {
                        if operand == constant {
                            out.push((user, j as u32, path.clone()));
                        }
                    }
                }
                LLVMConstantExprValueKind => {
                    let next = match ir.const_opcode(user) {
                        LLVMGetElementPtr if ir.operand(user, 0) == constant => {
                            shift(path.clone(), add(0, ir.gep_offset(user)).checked_neg().unwrap_or(ANY))
                        }
                        LLVMGetElementPtr => continue,
                        LLVMBitCast | LLVMPtrToInt | LLVMIntToPtr | LLVMAddrSpaceCast | LLVMTrunc => path.clone(),
                        _ => shift(path.clone(), ANY),
                    };
                    self.constant_uses(user, next, out, constructor, seen);
                }
                LLVMConstantStructValueKind | LLVMConstantArrayValueKind | LLVMConstantVectorValueKind => {
                    for (i, operand) in ir.operands(user).into_iter().enumerate() {
                        if operand != constant {
                            continue;
                        }
                        let offset = ir.element_offset(user, i as u32);
                        let next = if ir.is_aggregate(constant) && !path.is_empty() {
                            prepend(add(offset, path[0]), &path[1..])
                        } else {
                            prepend(offset, &path)
                        };
                        self.constant_uses(user, next, out, constructor, seen);
                    }
                }
                LLVMGlobalVariableValueKind => {
                    let name = ir.raw_name(user);
                    if name == "llvm.global_ctors" {
                        *constructor = true;
                    }
                    if name.starts_with("llvm.") {
                        continue;
                    }
                    let next =
                        if ir.is_aggregate(constant) && !path.is_empty() { path.clone() } else { prepend(0, &path) };
                    self.constant_uses(user, next, out, constructor, seen);
                }
                LLVMGlobalAliasValueKind => self.constant_uses(user, path.clone(), out, constructor, seen),
                _ => {}
            }
        }
    }

    fn function_uses(&mut self, function: V) -> Rc<Uses> {
        if let Some(uses) = self.uses.get(&function) {
            return uses.clone();
        }
        let ir = self.ir;
        let mut out = vec![];
        let mut uses = Uses::default();
        self.constant_uses(function, vec![], &mut out, &mut uses.constructor, &mut HashSet::new());
        for (inst, op, path) in out {
            let callee = ir.is_call(inst) && path.is_empty() && op + 1 == ir.operands(inst).len() as u32;
            if callee {
                uses.calls.push(inst);
                continue;
            }
            let key = (function, inst, op, path.clone());
            let id = *self.site_ids.entry(key).or_insert_with(|| {
                self.sites.push(Site { target: function, inst, op, path });
                self.sites.len() as u32 - 1
            });
            uses.sites.push(id);
        }
        let uses = Rc::new(uses);
        self.uses.insert(function, uses.clone());
        uses
    }

    // ---- Following a pointer forward from where it is taken. ----

    fn start_flow(&mut self, site: u32) {
        if self.flows.contains_key(&site) {
            return;
        }
        let Site { target, inst, op, ref path } = self.sites[site as usize];
        let path = path.clone();
        self.flows.insert(
            site,
            Flow { target, seen: HashSet::new(), uses: vec![], used: HashSet::new(), escaped: None },
        );
        self.work.push_back((site, Item::Use(inst, op, path)));
    }

    fn drain(&mut self) {
        let ir = self.ir;
        while let Some((flow, item)) = self.work.pop_front() {
            if self.flows[&flow].escaped.is_some() {
                continue;
            }
            let (value, path) = match item {
                Item::Fact(value, path) => (value, path),
                Item::Use(user, op, path) => {
                    self.visit_use(flow, user, op, path);
                    continue;
                }
                Item::Callers(function, cont) => {
                    self.callers(flow, function, cont);
                    continue;
                }
            };
            if ir.is_global(value) || ir.is_function(value) {
                let mut out = vec![];
                self.constant_uses(value, path, &mut out, &mut false, &mut HashSet::new());
                for (user, op, path) in out {
                    self.visit_use(flow, user, op, path);
                }
            } else {
                for user in ir.users(value) {
                    if !ir.is_instruction(user) || !self.live.contains(&ir.parent(user)) {
                        continue;
                    }
                    for (j, operand) in ir.operands(user).into_iter().enumerate() {
                        if operand == value {
                            self.visit_use(flow, user, j as u32, path.clone());
                        }
                    }
                }
            }
        }
    }

    fn fact(&mut self, flow: u32, value: V, path: Path) {
        let ir = self.ir;
        if !(ir.is_instruction(value) || ir.is_argument(value) || ir.is_global(value)) {
            return;
        }
        if path.len() > MAX_PATH {
            return self.escape(flow, "it is stored too many levels deep to follow".into());
        }
        let state = self.flows.get_mut(&flow).unwrap();
        if state.seen.len() >= MAX_FACTS {
            return self.escape(flow, "it reaches too many places to follow".into());
        }
        if state.seen.insert((value, path.clone())) {
            if self.trace {
                eprintln!("  fact[{flow}] {} {:?}", self.show(value), path);
            }
            self.work.push_back((flow, Item::Fact(value, path)));
        }
    }

    fn show(&self, value: V) -> String {
        let ir = self.ir;
        if ir.is_global(value) || ir.is_function(value) {
            return format!("@{}", ir.name(value));
        }
        let what = if ir.is_argument(value) {
            format!("argument {}", ir.param_index(value))
        } else {
            format!("{:?}", ir.opcode(value)).replace("LLVM", "").to_lowercase()
        };
        format!("{what} in {}", ir.name(ir.parent(value)))
    }

    fn escape(&mut self, flow: u32, reason: String) {
        let mut pending = vec![(flow, reason)];
        while let Some((flow, reason)) = pending.pop() {
            let state = self.flows.get_mut(&flow).unwrap();
            if state.escaped.is_some() {
                continue;
            }
            if self.trace {
                eprintln!("  escape[{flow}] {reason}");
            }
            state.escaped = Some(reason);
            // Whatever waits for this function's callers cannot know them all.
            let target = state.target;
            let name = self.ir.name(target);
            for (other, _) in self.subs.get(&target).into_iter().flatten() {
                pending.push((*other, format!("the callers of {name} cannot all be found")));
            }
        }
    }

    /// Records a call through the pointer (or a hand-over to outside code).
    fn consume(&mut self, flow: u32, call: V, external: bool) {
        let state = self.flows.get_mut(&flow).unwrap();
        if !state.used.insert(call) {
            return;
        }
        state.uses.push((call, external));
        let target = state.target;
        if self.trace {
            eprintln!("  use[{flow}] {} external={external}", self.show(call));
        }
        for (other, cont) in self.subs.get(&target).cloned().unwrap_or_default() {
            if external {
                let name = self.ir.name(target);
                self.escape(other, format!("{name} is called by code outside the module"));
            } else {
                self.apply(other, call, &cont);
            }
        }
    }

    fn apply(&mut self, flow: u32, call: V, cont: &Cont) {
        match cont {
            Cont::Ret(path) => self.fact(flow, call, path.clone()),
            Cont::Arg(index, path) => {
                if *index < self.ir.args(call) {
                    self.pointer_fact(flow, self.ir.operand(call, *index), path.clone(), 0);
                }
            }
        }
    }

    /// Carries something to every caller of `function`.
    fn to_callers(&mut self, flow: u32, function: V, cont: Cont) {
        if self.asked.insert((flow, function, cont.clone())) {
            self.work.push_back((flow, Item::Callers(function, cont)));
        }
    }

    fn callers(&mut self, flow: u32, function: V, cont: Cont) {
        let uses = self.function_uses(function);
        for &call in &uses.calls {
            self.apply(flow, call, &cont);
        }
        if uses.sites.is_empty() || !self.subscribed.insert((function, flow, cont.clone())) {
            return;
        }
        // Called through pointers as well: those calls are what the flows of
        // its own address find.
        self.subs.entry(function).or_default().push((flow, cont.clone()));
        for &site in &uses.sites {
            self.start_flow(site);
        }
        for &site in &uses.sites {
            let state = &self.flows[&site];
            let (escaped, found) = (state.escaped.clone(), state.uses.clone());
            let name = self.ir.name(function);
            if let Some(reason) = escaped {
                let _ = reason;
                self.escape(flow, format!("the callers of {name} cannot all be found"));
            }
            for (call, external) in found {
                if external {
                    self.escape(flow, format!("{name} is called by code outside the module"));
                } else {
                    self.apply(flow, call, &cont);
                }
            }
        }
    }

    /// The memory `pointer` points at holds the tracked pointer at `path`.
    /// Finds the object the pointer is into, so that every other pointer to
    /// that object sees it.
    fn pointer_fact(&mut self, flow: u32, mut pointer: V, mut path: Path, depth: u32) {
        use {LLVMOpcode::*, LLVMValueKind::*};
        let ir = self.ir;
        loop {
            match ir.kind(pointer) {
                LLVMInstructionValueKind => match ir.opcode(pointer) {
                    LLVMGetElementPtr => {
                        path = shift(path, ir.gep_offset(pointer));
                        pointer = ir.operand(pointer, 0);
                    }
                    LLVMBitCast | LLVMAddrSpaceCast => pointer = ir.operand(pointer, 0),
                    LLVMAlloca => return self.fact(flow, pointer, path),
                    LLVMPHI | LLVMSelect if depth < 8 => {
                        let skip = usize::from(ir.opcode(pointer) == LLVMSelect);
                        for operand in ir.operands(pointer).into_iter().skip(skip) {
                            self.pointer_fact(flow, operand, path.clone(), depth + 1);
                        }
                        return;
                    }
                    LLVMCall => {
                        let name = ir.called_function(pointer).map(|f| ir.raw_name(f)).unwrap_or_default();
                        if name.starts_with("llvm.threadlocal.address") {
                            pointer = ir.operand(pointer, 0);
                        } else if is_allocation(&name) {
                            return self.fact(flow, pointer, path);
                        } else {
                            let host = ir.name(ir.parent(pointer));
                            return self.escape(flow, format!("it is stored in memory that {host} gets from a call"));
                        }
                    }
                    _ => {
                        let host = ir.name(ir.parent(pointer));
                        return self.escape(flow, format!("it is stored in memory that {host} reaches through another pointer"));
                    }
                },
                LLVMArgumentValueKind => {
                    self.fact(flow, pointer, path.clone());
                    let index = ir.param_index(pointer);
                    return self.to_callers(flow, ir.parent(pointer), Cont::Arg(index, path));
                }
                LLVMGlobalVariableValueKind => return self.fact(flow, pointer, path),
                LLVMGlobalAliasValueKind => pointer = ir.operand(pointer, 0),
                LLVMConstantExprValueKind => match ir.const_opcode(pointer) {
                    LLVMGetElementPtr => {
                        path = shift(path, ir.gep_offset(pointer));
                        pointer = ir.operand(pointer, 0);
                    }
                    LLVMBitCast | LLVMAddrSpaceCast => pointer = ir.operand(pointer, 0),
                    _ => return self.escape(flow, "it is stored at a computed address".into()),
                },
                LLVMConstantPointerNullValueKind | LLVMUndefValueValueKind | LLVMPoisonValueKind => return,
                _ => return self.escape(flow, "it is stored at an address that cannot be followed".into()),
            }
        }
    }

    /// Operand `op` of `user` holds the tracked pointer at `path`.
    fn visit_use(&mut self, flow: u32, user: V, op: u32, path: Path) {
        use LLVMOpcode::*;
        let ir = self.ir;
        let first = path.first().copied();
        let rest = || path.get(1..).unwrap_or_default().to_vec();
        match ir.opcode(user) {
            LLVMGetElementPtr => {
                if op == 0 && !path.is_empty() {
                    let offset = ir.gep_offset(user);
                    self.fact(flow, user, shift(path, offset.checked_neg().unwrap_or(ANY)));
                }
            }
            LLVMBitCast | LLVMAddrSpaceCast | LLVMPtrToInt | LLVMIntToPtr | LLVMFreeze | LLVMTrunc | LLVMZExt
            | LLVMSExt | LLVMPHI => self.fact(flow, user, path),
            LLVMSelect => {
                if op != 0 {
                    self.fact(flow, user, path)
                }
            }
            LLVMLoad => {
                let Some(first) = first else { return };
                if ir.is_aggregate(user) {
                    if first == ANY || (0..ir.size(user)).contains(&first) {
                        self.fact(flow, user, path);
                    }
                } else if first == 0 || first == ANY {
                    self.fact(flow, user, rest());
                }
            }
            LLVMStore => {
                if op == 0 {
                    let stored = if ir.is_aggregate(ir.operand(user, 0)) && !path.is_empty() {
                        path
                    } else {
                        prepend(0, &path)
                    };
                    self.pointer_fact(flow, ir.operand(user, 1), stored, 0);
                }
            }
            LLVMCall | LLVMInvoke | LLVMCallBr => self.visit_call(flow, user, op, path),
            LLVMRet => self.to_callers(flow, ir.parent(user), Cont::Ret(path)),
            LLVMExtractValue => {
                let Some(first) = first else { return };
                let field = ir.indices_offset(user, ir.operand(user, 0));
                if ir.is_aggregate(user) {
                    if first == ANY || field == ANY {
                        self.fact(flow, user, shift(path, ANY));
                    } else if first >= field && first < field + ir.size(user) {
                        self.fact(flow, user, prepend(first - field, &path[1..]));
                    }
                } else if first == ANY || field == ANY || first == field {
                    self.fact(flow, user, rest());
                }
            }
            LLVMInsertValue => {
                if op == 0 {
                    self.fact(flow, user, path);
                } else if op == 1 {
                    let field = ir.indices_offset(user, user);
                    let inserted = if ir.is_aggregate(ir.operand(user, 1)) && !path.is_empty() {
                        prepend(add(field, path[0]), &path[1..])
                    } else {
                        prepend(field, &path)
                    };
                    self.fact(flow, user, inserted);
                }
            }
            LLVMAdd | LLVMSub | LLVMAnd | LLVMOr | LLVMXor | LLVMMul | LLVMShl | LLVMLShr | LLVMAShr => {
                self.fact(flow, user, shift(path, ANY))
            }
            LLVMAtomicCmpXchg => {
                if op == 2 {
                    self.pointer_fact(flow, ir.operand(user, 0), prepend(0, &path), 0);
                } else if op == 0 && matches!(first, Some(0 | ANY)) {
                    self.fact(flow, user, prepend(0, &path[1..]));
                }
            }
            LLVMAtomicRMW => {
                if op == 1 {
                    self.pointer_fact(flow, ir.operand(user, 0), prepend(0, &path), 0);
                } else if op == 0 && matches!(first, Some(0 | ANY)) {
                    self.fact(flow, user, rest());
                }
            }
            LLVMICmp | LLVMFCmp | LLVMCondBr | LLVMUncondBr | LLVMSwitch | LLVMUnreachable | LLVMIndirectBr => {}
            other => {
                let host = ir.name(ir.parent(user));
                self.escape(flow, format!("{host} uses it in a way that is not followed ({other:?})"));
            }
        }
    }

    fn visit_call(&mut self, flow: u32, call: V, op: u32, path: Path) {
        let ir = self.ir;
        if op + 1 == ir.operands(call).len() as u32 {
            // The callee.
            if path.is_empty() {
                self.consume(flow, call, false);
            }
            return;
        }
        if op >= ir.args(call) || ir.is_inline_asm(call) {
            return;
        }
        let Some(function) = ir.called_function(call) else {
            let host = ir.name(ir.parent(call));
            return self.escape(flow, format!("{host} passes it to a call made through another pointer"));
        };
        let raw = ir.raw_name(function);
        if ir.is_intrinsic(function) {
            if raw.starts_with("llvm.memcpy") || raw.starts_with("llvm.memmove") {
                if op == 1 && !path.is_empty() {
                    self.pointer_fact(flow, ir.operand(call, 0), path, 0);
                }
            } else if ["llvm.threadlocal.address", "llvm.launder", "llvm.strip", "llvm.ptrmask"]
                .iter()
                .any(|p| raw.starts_with(p))
            {
                if op == 0 {
                    self.fact(flow, call, path);
                }
            }
            return;
        }
        if ir.is_declaration(function) {
            let name = ir.name(function);
            if name.contains("__rust_realloc") || raw == "realloc" {
                if op == 0 {
                    self.fact(flow, call, path);
                }
            } else if !is_inert(&name) {
                self.consume(flow, call, true);
            }
            return;
        }
        match ir.params(function).get(op as usize) {
            Some(&param) => self.fact(flow, param, path),
            None => self.escape(flow, format!("it is passed to variadic {}", ir.name(function))),
        }
    }

    // ---- The conditions under which a sink is validated. ----

    fn no(&self, reason: impl Into<Rc<str>>) -> Node {
        Node::No(reason.into())
    }

    /// The node for a value used in `host`.
    fn value_node(&self, value: V, host: V) -> Node {
        let ir = self.ir;
        if ir.is_instruction(value) || ir.is_argument(value) {
            Node::Val(value)
        } else if ir.is_undef(value) {
            Node::Yes
        } else {
            // A constant is as validated as the code that uses it.
            Node::Fn(host)
        }
    }
    fn memory_node(&self, pointer: V) -> Node {
        let ir = self.ir;
        if ir.is_instruction(pointer) || ir.is_argument(pointer) {
            Node::Mem(pointer)
        } else {
            self.no("data is read from a global")
        }
    }

    /// The channel an instruction receives from, if it was inlined from a
    /// channel's receive function or calls one.
    fn receives(&self, inst: V) -> Option<String> {
        let ir = self.ir;
        for (sub, at) in ir.inline_chain(inst) {
            if at.is_null() {
                break;
            }
            if let Some(channel) = sub.as_ref().and_then(|s| s.channel.as_ref()) {
                if channel.receive {
                    return Some(channel.key.clone());
                }
            }
        }
        if ir.is_call(inst) {
            let channel = ir.called_function(inst).and_then(|f| ir::channel(&ir.name(f)))?;
            return channel.receive.then_some(channel.key);
        }
        None
    }

    fn return_values(&mut self, function: V) -> Rc<Vec<V>> {
        let ir = self.ir;
        self.returns
            .entry(function)
            .or_insert_with(|| {
                Rc::new(
                    ir.instructions(function)
                        .into_iter()
                        .filter(|&i| ir.opcode(i) == LLVMOpcode::LLVMRet && !ir.operands(i).is_empty())
                        .map(|i| ir.operand(i, 0))
                        .collect(),
                )
            })
            .clone()
    }

    /// The functions that send on each channel, outside validated code.
    fn send_hosts(&mut self, key: &str) -> Vec<V> {
        let ir = self.ir;
        if self.sends.is_none() {
            let mut sends: HashMap<String, HashSet<V>> = HashMap::new();
            let mut live: Vec<V> = self.live.iter().copied().collect();
            live.sort();
            for function in live {
                for inst in ir.instructions(function) {
                    let mut keys = vec![];
                    for (sub, at) in ir.inline_chain(inst) {
                        if at.is_null() {
                            break;
                        }
                        if let Some(channel) = sub.as_ref().and_then(|s| s.channel.as_ref()) {
                            if channel.send {
                                keys.push(channel.key.clone());
                            }
                        }
                    }
                    if ir.is_call(inst) {
                        if let Some(channel) = ir.called_function(inst).and_then(|f| ir::channel(&ir.name(f))) {
                            if channel.send {
                                keys.push(channel.key);
                            }
                        }
                    }
                    for key in keys {
                        // Validation differs per identifier, so keep the
                        // instruction's function and check the region later.
                        sends.entry(key).or_default().insert(inst);
                    }
                }
            }
            self.sends = Some(sends);
        }
        let (family, ty) = key.split_once(':').unwrap();
        let mut hosts = HashSet::new();
        for (other, insts) in self.sends.as_ref().unwrap() {
            let (other_family, other_ty) = other.split_once(':').unwrap();
            if family != other_family || !(ty == other_ty || ty == "?" || other_ty == "?") {
                continue;
            }
            for &inst in insts {
                if !self.validated_inst(inst) {
                    hosts.insert(ir.parent(inst));
                }
            }
        }
        let mut hosts: Vec<V> = hosts.into_iter().collect();
        hosts.sort();
        hosts
    }

    fn expand(&mut self, node: &Node) -> Expr {
        use LLVMOpcode::*;
        let ir = self.ir;
        match node {
            Node::Yes => Expr::True,
            Node::No(reason) => Expr::False(reason.clone()),
            Node::Flow(_) | Node::Data(_) => Expr::Pending,
            &Node::Fn(function) => {
                if self.validators.contains(&function) {
                    return Expr::True;
                }
                if Some(function) == self.entry {
                    return Expr::False("the program's entry point is reached without validation".into());
                }
                let name = ir.name(function);
                if self.known_functions.contains(&*name) {
                    self.ignored += 1;
                    return Expr::True;
                }
                let known = |analysis: &Self, host: V| {
                    !analysis.known_calls.is_empty()
                        && analysis.known_calls.contains(&(name.to_string(), ir.name(host).to_string()))
                };
                let uses = self.function_uses(function);
                let sink = self.sinks.contains(&function) && !ir.params(function).is_empty();
                let mut children = vec![];
                for &call in &uses.calls {
                    if self.validated_inst(call) {
                        continue;
                    }
                    if known(self, ir.parent(call)) {
                        self.ignored += 1;
                        continue;
                    }
                    children.push(if sink { Node::SinkCall(call) } else { Node::Fn(ir.parent(call)) });
                }
                for &site in &uses.sites {
                    if known(self, ir.parent(self.sites[site as usize].inst)) {
                        self.ignored += 1;
                        continue;
                    }
                    children.push(Node::Creation(site));
                }
                if uses.constructor {
                    children.push(self.no("it runs as a static constructor"));
                }
                Expr::All(children)
            }
            &Node::Creation(site) => {
                let inst = self.sites[site as usize].inst;
                if self.validated_inst(inst) {
                    return Expr::True;
                }
                Expr::Any(vec![Node::Fn(ir.parent(inst)), Node::Flow(site)])
            }
            &Node::SinkCall(call) => Expr::Any(vec![Node::Fn(ir.parent(call)), Node::Data(call)]),
            &Node::ArgAt(call, index) => {
                if self.validated_inst(call) {
                    return Expr::True;
                }
                let host = ir.parent(call);
                Expr::Any(vec![Node::Fn(host), self.value_node(ir.operand(call, index), host)])
            }
            &Node::ArgMem(call, index) => {
                if self.validated_inst(call) {
                    return Expr::True;
                }
                Expr::Any(vec![Node::Fn(ir.parent(call)), self.memory_node(ir.operand(call, index))])
            }
            &Node::Val(value) => {
                let host = ir.parent(value);
                if self.validators.contains(&host) || (ir.is_instruction(value) && self.validated_inst(value)) {
                    return Expr::True;
                }
                Expr::Any(vec![Node::Fn(host), Node::Operands(value)])
            }
            Node::Sends(key) => Expr::All(self.send_hosts(key).into_iter().map(Node::Fn).collect()),
            &Node::Operands(value) => {
                self.data_nodes += 1;
                if self.data_nodes > MAX_DATA_NODES {
                    return Expr::False("the data comes from too many places to follow".into());
                }
                let host = ir.parent(value);
                if ir.is_argument(value) {
                    let uses = self.function_uses(host);
                    let index = ir.param_index(value);
                    let mut children: Vec<Node> = uses.calls.iter().map(|&call| Node::ArgAt(call, index)).collect();
                    if !uses.sites.is_empty() {
                        children.push(self.no(format!(
                            "data is an argument of {}, which is called through a pointer",
                            ir.name(host)
                        )));
                    }
                    return Expr::All(children);
                }
                if let Some(key) = self.receives(value) {
                    return Expr::All(vec![Node::Sends(key)]);
                }
                let operands = ir.operands(value);
                match ir.opcode(value) {
                    LLVMCall | LLVMInvoke | LLVMCallBr => {
                        if ir.is_inline_asm(value) {
                            return Expr::All(vec![self.no("data comes from inline assembly")]);
                        }
                        let Some(function) = ir.called_function(value) else {
                            return Expr::All(vec![self.no(format!(
                                "data is the result of a call through a pointer in {}",
                                ir.name(host)
                            ))]);
                        };
                        if self.validators.contains(&function) {
                            return Expr::True;
                        }
                        if ir.is_intrinsic(function) {
                            let args = ir.args(value) as usize;
                            return Expr::All(
                                operands[..args]
                                    .iter()
                                    .filter(|&&o| !ir.is_constant(o))
                                    .map(|&o| self.value_node(o, host))
                                    .collect(),
                            );
                        }
                        if ir.is_declaration(function) {
                            return Expr::All(vec![self.no(format!(
                                "data is the result of {}, which is outside the module",
                                ir.name(function)
                            ))]);
                        }
                        let returns = self.return_values(function);
                        Expr::All(returns.iter().map(|&v| self.value_node(v, function)).collect())
                    }
                    LLVMLoad => Expr::All(vec![self.memory_node(operands[0])]),
                    LLVMPHI => Expr::All(operands.iter().map(|&o| self.value_node(o, host)).collect()),
                    LLVMAlloca => Expr::All(vec![self.no("data is the address of a local")]),
                    _ => Expr::All(
                        operands
                            .iter()
                            .filter(|&&o| ir.is_instruction(o) || ir.is_argument(o))
                            .map(|&o| self.value_node(o, host))
                            .collect(),
                    ),
                }
            }
            &Node::Mem(pointer) => {
                self.data_nodes += 1;
                if self.data_nodes > MAX_DATA_NODES {
                    return Expr::False("the data comes from too many places to follow".into());
                }
                // The object the pointer is into.
                let mut base = pointer;
                while ir.is_instruction(base)
                    && matches!(ir.opcode(base), LLVMGetElementPtr | LLVMBitCast | LLVMAddrSpaceCast)
                {
                    base = ir.operand(base, 0);
                }
                let host = ir.parent(pointer);
                let local = ir.is_instruction(base) && ir.opcode(base) == LLVMAlloca;
                if !local && !ir.is_argument(base) {
                    if ir.is_instruction(base) {
                        if let Some(key) = self.receives(base) {
                            return Expr::All(vec![Node::Sends(key)]);
                        }
                    }
                    return Expr::All(vec![self.no(format!(
                        "data is read from memory that {} reaches through another pointer",
                        ir.name(host)
                    ))]);
                }
                let mut children = vec![];
                // Everything written through a pointer into the object.
                let mut derived = vec![base];
                let mut seen: HashSet<V> = derived.iter().copied().collect();
                while let Some(address) = derived.pop() {
                    for user in ir.users(address) {
                        if !ir.is_instruction(user) {
                            continue;
                        }
                        let operands = ir.operands(user);
                        match ir.opcode(user) {
                            LLVMGetElementPtr | LLVMBitCast | LLVMAddrSpaceCast => {
                                if operands[0] == address && seen.insert(user) {
                                    derived.push(user);
                                }
                            }
                            LLVMStore if operands[1] == address => {
                                if self.validated_inst(user) {
                                    continue;
                                }
                                match self.receives(user) {
                                    Some(key) => children.push(Node::Sends(key)),
                                    None => children.push(self.value_node(operands[0], host)),
                                }
                            }
                            LLVMCall | LLVMInvoke | LLVMCallBr => {
                                if ir.is_inline_asm(user) || self.validated_inst(user) {
                                    continue;
                                }
                                if let Some(key) = self.receives(user) {
                                    children.push(Node::Sends(key));
                                    continue;
                                }
                                let Some(function) = ir.called_function(user) else {
                                    children.push(self.no("data is written by a call through a pointer"));
                                    continue;
                                };
                                if self.validators.contains(&function) {
                                    continue;
                                }
                                let raw = ir.raw_name(function);
                                if ir.is_intrinsic(function) {
                                    if (raw.starts_with("llvm.memcpy") || raw.starts_with("llvm.memmove"))
                                        && operands[0] == address
                                    {
                                        children.push(self.memory_node(operands[1]));
                                    }
                                    continue;
                                }
                                for index in 0..ir.args(user) {
                                    if operands[index as usize] != address {
                                        continue;
                                    }
                                    if ir.is_declaration(function) {
                                        if !is_inert(&ir.name(function)) {
                                            children.push(self.no(format!(
                                                "data may be written by {}, which is outside the module",
                                                ir.name(function)
                                            )));
                                        }
                                    } else if let Some(&param) = ir.params(function).get(index as usize) {
                                        children.push(Node::Mem(param));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if ir.is_argument(base) {
                    let uses = self.function_uses(host);
                    let index = ir.param_index(base);
                    children.extend(uses.calls.iter().map(|&call| Node::ArgMem(call, index)));
                    if !uses.sites.is_empty() {
                        children.push(self.no(format!(
                            "data is behind an argument of {}, which is called through a pointer",
                            ir.name(host)
                        )));
                    }
                }
                Expr::All(children)
            }
        }
    }

    /// Works out a node that was left pending.
    fn resolve(&mut self, node: &Node) -> Expr {
        let ir = self.ir;
        match node {
            &Node::Flow(site) => {
                if self.trace {
                    let s = &self.sites[site as usize];
                    eprintln!("flow[{site}] of {} from {} {:?}", ir.name(s.target), self.show(s.inst), s.path);
                }
                self.start_flow(site);
                self.drain();
                let state = &self.flows[&site];
                if let Some(reason) = &state.escaped {
                    return Expr::False(reason.as_str().into());
                }
                let mut children = vec![];
                for &(call, _) in &state.uses {
                    if !self.validated_inst(call) {
                        children.push(Node::Fn(ir.parent(call)));
                    }
                }
                Expr::All(children)
            }
            &Node::Data(call) => {
                let host = ir.parent(call);
                Expr::All((0..ir.args(call)).map(|i| self.value_node(ir.operand(call, i), host)).collect())
            }
            _ => unreachable!(),
        }
    }

    /// Adds every node reachable from `from` to the graph.
    fn build(&mut self, from: Node) {
        let mut pending = vec![from];
        while let Some(node) = pending.pop() {
            if self.graph.contains_key(&node) {
                continue;
            }
            let expr = self.expand(&node);
            if let Expr::All(children) | Expr::Any(children) = &expr {
                pending.extend(children.iter().cloned());
            }
            self.graph.insert(node, expr);
        }
    }

    /// The nodes that do not hold: the least set closed under the rules, so a
    /// cycle of calls with no unvalidated way in holds.
    fn solve(&self) -> HashSet<Node> {
        let mut parents: HashMap<&Node, Vec<&Node>> = HashMap::new();
        let mut missing: HashMap<&Node, usize> = HashMap::new();
        let mut queue = vec![];
        for (node, expr) in &self.graph {
            match expr {
                Expr::True => {}
                Expr::False(_) | Expr::Pending => queue.push(node),
                Expr::All(children) => children.iter().for_each(|c| parents.entry(c).or_default().push(node)),
                Expr::Any(children) => {
                    children.iter().for_each(|c| parents.entry(c).or_default().push(node));
                    missing.insert(node, children.len());
                    if children.is_empty() {
                        queue.push(node);
                    }
                }
            }
        }
        let mut failed: HashSet<Node> = HashSet::new();
        while let Some(node) = queue.pop() {
            if !failed.insert(node.clone()) {
                continue;
            }
            for &parent in parents.get(node).into_iter().flatten() {
                match missing.get_mut(parent) {
                    Some(left) => {
                        *left -= 1;
                        if *left == 0 {
                            queue.push(parent);
                        }
                    }
                    None => queue.push(parent),
                }
            }
        }
        failed
    }

    /// Checks one sink function. Returns the failing chain, if any.
    fn check(&mut self, sink: V) -> Option<Vec<String>> {
        let root = Node::Fn(sink);
        self.build(root.clone());
        loop {
            let failed = self.solve();
            if !failed.contains(&root) {
                return None;
            }
            // Pending nodes that the failure depends on.
            let mut wanted = vec![];
            let mut seen = HashSet::new();
            let mut pending = vec![&root];
            while let Some(node) = pending.pop() {
                if !failed.contains(node) || !seen.insert(node) {
                    continue;
                }
                match &self.graph[node] {
                    Expr::Pending => wanted.push(node.clone()),
                    Expr::All(children) | Expr::Any(children) => pending.extend(children),
                    _ => {}
                }
            }
            if wanted.is_empty() {
                return Some(self.explain(&root, &failed));
            }
            // Data first: it is cheaper than following pointers.
            wanted.sort_by_key(|n| !matches!(n, Node::Data(_)));
            let data = matches!(wanted[0], Node::Data(_));
            for node in wanted {
                if data != matches!(node, Node::Data(_)) {
                    break;
                }
                let expr = self.resolve(&node);
                if let Expr::All(children) | Expr::Any(children) = &expr {
                    for child in children.clone() {
                        self.build(child);
                    }
                }
                self.graph.insert(node, expr);
            }
        }
    }

    fn describe(&self, node: &Node) -> Option<String> {
        let ir = self.ir;
        Some(match node {
            &Node::Fn(function) => format!("<- {}", ir.name(function)),
            &Node::Creation(site) => {
                let site = &self.sites[site as usize];
                format!("   (its address is taken in {})", ir.name(ir.parent(site.inst)))
            }
            Node::Flow(_) => "   (not every call through that pointer is validated)".into(),
            Node::Data(_) => "   (not every argument is validated data)".into(),
            Node::Sends(key) => format!("   (not every send of {} is validated)", key.replace(':', " channel message ")),
            _ => return None,
        })
    }

    /// The first reason found under a failed node.
    fn reason(&self, node: &Node, failed: &HashSet<Node>) -> Option<String> {
        let mut seen = HashSet::new();
        let mut pending = vec![node];
        while let Some(node) = pending.pop() {
            if !failed.contains(node) || !seen.insert(node) {
                continue;
            }
            match &self.graph[node] {
                Expr::False(reason) => return Some(reason.to_string()),
                Expr::All(children) | Expr::Any(children) => pending.extend(children.iter().rev()),
                _ => {}
            }
        }
        None
    }

    fn explain(&self, root: &Node, failed: &HashSet<Node>) -> Vec<String> {
        let mut lines = vec![];
        let mut seen = HashSet::new();
        let mut node = root.clone();
        loop {
            if !seen.insert(node.clone()) {
                break;
            }
            lines.extend(self.describe(&node));
            match &self.graph[&node] {
                Expr::False(reason) => {
                    lines.push(format!("   {reason}"));
                    break;
                }
                Expr::All(children) => match children.iter().find(|c| failed.contains(c)) {
                    Some(child) => node = child.clone(),
                    None => break,
                },
                Expr::Any(children) => {
                    // Follow the callers; say why the alternative fails too.
                    for other in &children[1..] {
                        lines.extend(self.describe(other));
                        if let Some(reason) = self.reason(other, failed) {
                            lines.push(format!("     {reason}"));
                        }
                    }
                    match children.first() {
                        Some(child) => node = child.clone(),
                        None => break,
                    }
                }
                _ => break,
            }
        }
        lines
    }
}

fn is_allocation(raw: &str) -> bool {
    raw.contains("__rust_alloc") || raw.contains("__rust_realloc") || matches!(raw, "malloc" | "calloc" | "realloc")
}

/// Outside functions that never call a pointer they are given.
fn is_inert(name: &str) -> bool {
    const INTEGERS: [&str; 16] = [
        "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "bool", "char", "f32",
        "f64",
    ];
    name.contains("__rust_dealloc")
        || name.contains("__rust_alloc")
        || name.contains("::panicking::")
        || matches!(name, "free" | "memcmp" | "bcmp" | "strlen" | "memchr" | "write" | "read")
        || INTEGERS.iter().any(|ty| name.strip_prefix('<').and_then(|n| n.strip_prefix(ty)).is_some_and(|n| n.starts_with(" as core::fmt::")))
}

/// The identifier after `__<kind>__` in a marker function's name.
fn marker(name: &str) -> Option<(&'static str, String)> {
    for kind in ["validate", "sink", "source"] {
        if let Some((_, id)) = name.rsplit_once(&format!("__{kind}__")) {
            let id: String = id.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
            return Some((kind, id));
        }
    }
    None
}

fn main() -> ExitCode {
    // Constant initializers nest deeply in large modules.
    std::thread::Builder::new().stack_size(1 << 30).spawn(run).unwrap().join().unwrap()
}

/// Reads known false positives: blocks separated by blank lines, one
/// demangled function name per line, `#` comments, and a leading `<- ` or
/// `-> ` ignored so that chains can be pasted from the output. A block of one
/// name stops every chain at that function; a block of two stops the chain at
/// that call, whichever way round it is written.
fn known_issues(path: &str) -> Result<(HashSet<String>, HashSet<(String, String)>), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let mut functions = HashSet::new();
    let mut calls = HashSet::new();
    let mut block: Vec<String> = vec![];
    for line in text.lines().map(str::trim).chain([""]) {
        if line.starts_with('#') {
            continue;
        }
        let line = line.trim_start_matches("<- ").trim_start_matches("-> ");
        if !line.is_empty() {
            block.push(line.to_string());
            continue;
        }
        match std::mem::take(&mut block).as_slice() {
            [] => {}
            [function] => {
                functions.insert(function.clone());
            }
            [a, b] => {
                calls.insert((a.clone(), b.clone()));
                calls.insert((b.clone(), a.clone()));
            }
            longer => {
                return Err(format!(
                    "{path}: a known issue is one function or one call (two functions), not {}: {}",
                    longer.len(),
                    longer[0]
                ));
            }
        }
    }
    Ok((functions, calls))
}

fn run() -> ExitCode {
    let mut trace = false;
    let mut known = None;
    let mut paths = vec![];
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--trace" => trace = true,
            "--known-issues" => known = args.next(),
            _ => paths.push(arg),
        }
    }
    let [path] = &paths[..] else {
        eprintln!("usage: tinted-ir [--known-issues FILE] [--trace] PROGRAM.bc|PROGRAM.ll");
        return ExitCode::from(2);
    };
    let known = match known.as_deref().map(known_issues).transpose() {
        Ok(known) => known.unwrap_or_default(),
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let ir = match Ir::load(path) {
        Ok(ir) => ir,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let mut analysis = Analysis::new(&ir, trace);
    (analysis.known_functions, analysis.known_calls) = known;
    if analysis.entry.is_none() {
        println!("FAIL: no main entry point");
        return ExitCode::FAILURE;
    }

    // id -> (validate functions, places a validate function was inlined, sinks)
    #[derive(Default)]
    struct Marked {
        validators: HashSet<V>,
        inlined: HashSet<ir::Md>,
        sinks: Vec<V>,
    }
    let mut ids: BTreeMap<String, Marked> = BTreeMap::new();
    for function in ir.functions() {
        let raw = ir.raw_name(function);
        if !raw.contains("__validate__") && !raw.contains("__sink__") {
            continue;
        }
        let Some((kind, id)) = marker(&ir.name(function)) else { continue };
        let marked = ids.entry(id).or_default();
        for user in ir.users(function) {
            if !ir.is_call(user) || ir.called_function(user) != Some(function) {
                continue;
            }
            let host = ir.parent(user);
            let location = ir.location(user);
            let inlined = if location.is_null() { location } else { ir.inlined_at(location) };
            match kind {
                "validate" if !inlined.is_null() => {
                    marked.inlined.insert(inlined);
                }
                "validate" => {
                    marked.validators.insert(host);
                }
                "sink" if !marked.sinks.contains(&host) => marked.sinks.push(host),
                _ => {}
            }
        }
    }
    ids.retain(|_, marked| !marked.sinks.is_empty());
    if ids.is_empty() {
        println!("FAIL: no sink annotations");
        return ExitCode::FAILURE;
    }

    let mut ok = true;
    for (id, marked) in ids {
        println!("[{id}]");
        analysis.validators = marked.validators;
        analysis.inlined_validators = marked.inlined;
        analysis.sinks = marked.sinks.iter().copied().collect();
        analysis.graph.clear();
        analysis.data_nodes = 0;
        analysis.ignored = 0;
        for &validator in &analysis.validators {
            println!("  validate: {}", ir.name(validator));
        }
        if !analysis.inlined_validators.is_empty() {
            println!("  validate: {} inlined place(s)", analysis.inlined_validators.len());
        }
        let mut reachable = 0;
        for &sink in &marked.sinks {
            let name = ir.name(sink);
            if !analysis.live.contains(&sink) {
                println!("  sink: {name} (not reachable from main)");
                continue;
            }
            reachable += 1;
            match analysis.check(sink) {
                None => println!("  sink: {name}: OK"),
                Some(chain) => {
                    ok = false;
                    println!("  sink: {name}: FAIL: unchecked sink");
                    for line in chain {
                        println!("    {line}");
                    }
                }
            }
        }
        if reachable == 0 {
            println!("  FAIL: no reachable sinks");
            ok = false;
        }
        if analysis.ignored > 0 {
            println!("  note: {} chain(s) stopped at a known issue", analysis.ignored);
        }
        println!(
            "  explored {} condition(s), followed {} pointer(s)",
            analysis.graph.len(),
            analysis.flows.len()
        );
    }
    println!("{}", if ok { "OK" } else { "FAIL" });
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
