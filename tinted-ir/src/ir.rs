//! Thin safe-looking wrappers over the LLVM C API. Every value handed out
//! belongs to the module owned by `Ir`, which lives for the whole run.

use {
    llvm_sys::{LLVMOpcode, LLVMTypeKind, LLVMValueKind, core::*, debuginfo::*, prelude::*, target::*},
    std::{cell::RefCell, collections::HashMap, ffi::CString, rc::Rc},
};

pub type V = LLVMValueRef;
pub type Md = LLVMMetadataRef;
pub type B = LLVMBasicBlockRef;
/// An unknown byte offset.
pub const ANY: i64 = i64::MIN;

pub struct Ir {
    pub ctx: LLVMContextRef,
    pub module: LLVMModuleRef,
    td: LLVMTargetDataRef,
    names: RefCell<HashMap<V, Rc<str>>>,
    scopes: RefCell<HashMap<Md, Option<Rc<Sub>>>>,
}

/// A debug-info subprogram: a function as the source names it.
pub struct Sub {
    pub channel: Option<Channel>,
}

/// A summarized channel operation: `send`, `receive`, or both, and the
/// channel's family and message type.
#[derive(Clone)]
pub struct Channel {
    pub send: bool,
    pub receive: bool,
    pub key: String,
}

pub fn demangle(name: &str) -> String {
    format!("{:#}", rustc_demangle::demangle(name))
}

/// The first generic argument that starts at `from` in `name`.
fn first_argument(name: &str, from: usize) -> &str {
    let mut nesting = 0;
    for (i, c) in name[from..].char_indices() {
        match c {
            '<' | '(' | '[' => nesting += 1,
            ',' | '>' if nesting == 0 => return &name[from..from + i],
            '>' | ')' | ']' => nesting -= 1,
            _ => {}
        }
    }
    &name[from..]
}

/// Classifies a demangled function name as a channel send or receive of
/// `std::sync::mpsc` (and the `mpmc` code under it), `crossbeam_channel`,
/// `tokio::sync::{mpsc, oneshot, broadcast, watch}`,
/// `futures_channel::{mpsc, oneshot}`, `flume` or `async_channel`.
pub fn channel(name: &str) -> Option<Channel> {
    let within = |module: &str| {
        name.strip_prefix('<').unwrap_or(name).starts_with(module)
    };
    let family = if within("std::sync::mpsc::") || within("std::sync::mpmc::") {
        "std"
    } else if within("crossbeam_channel::") {
        "crossbeam"
    } else if within("tokio::sync::mpsc::") {
        "tokio mpsc"
    } else if within("tokio::sync::oneshot::") {
        "tokio oneshot"
    } else if within("tokio::sync::broadcast::") {
        "tokio broadcast"
    } else if within("tokio::sync::watch::") {
        "tokio watch"
    } else if within("flume::") {
        "flume"
    } else if within("async_channel::") {
        "async-channel"
    } else if within("futures_channel::mpsc::") {
        "futures mpsc"
    } else if within("futures_channel::oneshot::") {
        "futures oneshot"
    } else {
        return None;
    };
    // The async channels add poll-based and blocking variants, and the queue
    // operations that the public functions are inlined down to.
    let asynchronous = !matches!(family, "std" | "crossbeam");
    let mut send = ["::send", "::try_send", "::write"].iter().any(|p| name.contains(p));
    let mut receive = ["::recv", "::try_recv", "::read"].iter().any(|p| name.contains(p))
        || name.ends_with("::next");
    if asynchronous {
        send |= ["::blocking_send", "::start_send", "::unbounded_send", "::do_send", "::push", "::queue_push"]
            .iter()
            .any(|p| name.contains(p));
        receive |= [
            "::blocking_recv",
            "::poll_recv",
            "::poll_next",
            "::next_message",
            "::try_next",
            "::pop",
            // A oneshot receiver is itself the future that yields the value.
            "Receiver<",
        ]
        .iter()
        .any(|p| match *p {
            "Receiver<" => family.ends_with("oneshot") && name.contains("Receiver<") && name.ends_with("::poll"),
            p => name.contains(p),
        });
    }
    // The futures that an async send or receive returns do the work in their
    // `poll`; their destructors and other methods are not sends or receives.
    let polls = |future: &str| name.contains(future) && name.contains("::poll");
    match family {
        // A watch receiver reads the current value by borrowing it.
        "tokio watch" => receive |= name.contains("::borrow"),
        "flume" => {
            send |= polls("::SendFut<");
            receive |= polls("::RecvFut<") || polls("::RecvStream<");
        }
        "async-channel" => {
            send |= polls("::SendInner<") || name.contains("::force_send");
            receive |= polls("::RecvInner<");
        }
        _ => {}
    }
    if !send && !receive {
        return None;
    }
    let mut ty = name;
    let mut found = false;
    loop {
        let at = [
            "Channel<",
            "Sender<",
            "Receiver<",
            "IntoIter<",
            "TryIter<",
            "Iter<",
            // tokio's and futures' internal halves; qualified, so that a
            // message type such as `SignedTx<..>` is not taken for one.
            "::Tx<",
            "::Rx<",
            "::Chan<",
            "::Permit<",
            "::OwnedPermit<",
            "::Inner<",
            "::SenderInner<",
            "::UnboundedInner<",
            "::BoundedInner<",
            "::Queue<",
            "::Shared<",
            "::Ref<",
            "::SendFut<",
            "::RecvFut<",
            "::RecvStream<",
            "::SendSink<",
            "::SendInner<",
            "::RecvInner<",
            "::Send<",
            "::Recv<",
        ]
        .iter()
        .filter_map(|p| ty.find(p).map(|i| i + p.len()))
        .min();
        match at {
            Some(at) => {
                ty = first_argument(ty, at);
                found = true;
            }
            None => break,
        }
    }
    if !found {
        ty = name.rfind("::<").map_or("?", |at| first_argument(name, at + 3));
    }
    Some(Channel { send, receive, key: format!("{family}:{ty}") })
}

impl Ir {
    pub fn load(path: &str) -> Result<Self, String> {
        unsafe {
            let ctx = LLVMContextCreate();
            let cpath = CString::new(path).unwrap();
            let mut buffer = std::ptr::null_mut();
            let mut message = std::ptr::null_mut();
            if LLVMCreateMemoryBufferWithContentsOfFile(cpath.as_ptr(), &mut buffer, &mut message) != 0 {
                return Err(format!("cannot read {path}: {}", cstr(message)));
            }
            let mut module = std::ptr::null_mut();
            // Takes ownership of the buffer.
            #[allow(deprecated)]
            let failed = llvm_sys::ir_reader::LLVMParseIRInContext(ctx, buffer, &mut module, &mut message) != 0;
            if failed {
                return Err(format!("cannot parse {path}: {}", cstr(message)));
            }
            Ok(Self {
                ctx,
                module,
                td: LLVMGetModuleDataLayout(module),
                names: RefCell::default(),
                scopes: RefCell::default(),
            })
        }
    }

    pub fn functions(&self) -> Vec<V> {
        let mut all = vec![];
        unsafe {
            let mut f = LLVMGetFirstFunction(self.module);
            while !f.is_null() {
                all.push(f);
                f = LLVMGetNextFunction(f);
            }
        }
        all
    }

    pub fn function(&self, name: &str) -> Option<V> {
        let name = CString::new(name).unwrap();
        let f = unsafe { LLVMGetNamedFunction(self.module, name.as_ptr()) };
        (!f.is_null()).then_some(f)
    }

    pub fn instructions(&self, function: V) -> Vec<V> {
        let mut all = vec![];
        unsafe {
            let mut block = LLVMGetFirstBasicBlock(function);
            while !block.is_null() {
                let mut inst = LLVMGetFirstInstruction(block);
                while !inst.is_null() {
                    all.push(inst);
                    inst = LLVMGetNextInstruction(inst);
                }
                block = LLVMGetNextBasicBlock(block);
            }
        }
        all
    }

    pub fn raw_name(&self, v: V) -> String {
        unsafe {
            let mut len = 0;
            let ptr = LLVMGetValueName2(v, &mut len);
            String::from_utf8_lossy(std::slice::from_raw_parts(ptr.cast(), len)).into_owned()
        }
    }

    /// The demangled name of a function or global.
    pub fn name(&self, v: V) -> Rc<str> {
        if let Some(name) = self.names.borrow().get(&v) {
            return name.clone();
        }
        let name: Rc<str> = demangle(&self.raw_name(v)).into();
        self.names.borrow_mut().insert(v, name.clone());
        name
    }

    pub fn kind(&self, v: V) -> LLVMValueKind {
        unsafe { LLVMGetValueKind(v) }
    }
    pub fn is_instruction(&self, v: V) -> bool {
        self.kind(v) == LLVMValueKind::LLVMInstructionValueKind
    }
    pub fn is_function(&self, v: V) -> bool {
        self.kind(v) == LLVMValueKind::LLVMFunctionValueKind
    }
    pub fn is_global(&self, v: V) -> bool {
        self.kind(v) == LLVMValueKind::LLVMGlobalVariableValueKind
    }
    pub fn is_argument(&self, v: V) -> bool {
        self.kind(v) == LLVMValueKind::LLVMArgumentValueKind
    }
    pub fn is_constant(&self, v: V) -> bool {
        unsafe { !LLVMIsAConstant(v).is_null() }
    }
    pub fn is_undef(&self, v: V) -> bool {
        matches!(self.kind(v), LLVMValueKind::LLVMUndefValueValueKind | LLVMValueKind::LLVMPoisonValueKind)
    }
    pub fn is_declaration(&self, f: V) -> bool {
        unsafe { LLVMIsDeclaration(f) != 0 }
    }
    pub fn is_intrinsic(&self, f: V) -> bool {
        unsafe { LLVMGetIntrinsicID(f) != 0 }
    }
    pub fn opcode(&self, inst: V) -> LLVMOpcode {
        unsafe { LLVMGetInstructionOpcode(inst) }
    }
    pub fn const_opcode(&self, expr: V) -> LLVMOpcode {
        unsafe { LLVMGetConstOpcode(expr) }
    }
    pub fn is_call(&self, inst: V) -> bool {
        self.is_instruction(inst)
            && matches!(self.opcode(inst), LLVMOpcode::LLVMCall | LLVMOpcode::LLVMInvoke | LLVMOpcode::LLVMCallBr)
    }
    pub fn operands(&self, v: V) -> Vec<V> {
        unsafe { (0..LLVMGetNumOperands(v) as u32).map(|i| LLVMGetOperand(v, i)).collect() }
    }
    pub fn operand(&self, v: V, i: u32) -> V {
        unsafe { LLVMGetOperand(v, i) }
    }
    /// The first block of a function, or null for a declaration.
    pub fn entry_block(&self, function: V) -> B {
        unsafe { LLVMGetFirstBasicBlock(function) }
    }
    pub fn block_instructions(&self, block: B) -> Vec<V> {
        let mut all = vec![];
        unsafe {
            let mut inst = LLVMGetFirstInstruction(block);
            while !inst.is_null() {
                all.push(inst);
                inst = LLVMGetNextInstruction(inst);
            }
        }
        all
    }
    pub fn successors(&self, block: B) -> Vec<B> {
        unsafe {
            let terminator = LLVMGetBasicBlockTerminator(block);
            if terminator.is_null() {
                return vec![];
            }
            (0..LLVMGetNumSuccessors(terminator)).map(|i| LLVMGetSuccessor(terminator, i)).collect()
        }
    }
    /// The source file and line of an instruction, as `file:line`.
    pub fn source(&self, inst: V) -> Option<String> {
        let location = self.location(inst);
        if location.is_null() {
            return None;
        }
        unsafe {
            let line = LLVMDILocationGetLine(location);
            let scope = LLVMDILocationGetScope(location);
            if scope.is_null() {
                return None;
            }
            let file = LLVMDIScopeGetFile(scope);
            if file.is_null() {
                return None;
            }
            let mut len = 0;
            let ptr = LLVMDIFileGetFilename(file, &mut len);
            if ptr.is_null() || len == 0 {
                return None;
            }
            let name = String::from_utf8_lossy(std::slice::from_raw_parts(ptr.cast(), len as usize));
            Some(format!("{name}:{line}"))
        }
    }
    /// The call sites an instruction was inlined through, innermost first.
    /// `None` without a debug location.
    pub fn inlined_through(&self, inst: V) -> Option<Vec<Md>> {
        let mut location = self.location(inst);
        if location.is_null() {
            return None;
        }
        let mut chain = vec![];
        loop {
            location = self.inlined_at(location);
            if location.is_null() {
                return Some(chain);
            }
            chain.push(location);
        }
    }
    /// The function an instruction or argument is in.
    pub fn parent(&self, v: V) -> V {
        unsafe {
            if self.is_argument(v) {
                LLVMGetParamParent(v)
            } else {
                LLVMGetBasicBlockParent(LLVMGetInstructionParent(v))
            }
        }
    }
    pub fn params(&self, f: V) -> Vec<V> {
        unsafe { (0..LLVMCountParams(f)).map(|i| LLVMGetParam(f, i)).collect() }
    }
    pub fn param_index(&self, argument: V) -> u32 {
        self.params(self.parent(argument)).iter().position(|&p| p == argument).unwrap() as u32
    }
    /// The users of a value, each once.
    pub fn users(&self, v: V) -> Vec<V> {
        let mut users = vec![];
        let mut seen = std::collections::HashSet::new();
        unsafe {
            let mut u = LLVMGetFirstUse(v);
            while !u.is_null() {
                let user = LLVMGetUser(u);
                if seen.insert(user) {
                    users.push(user);
                }
                u = LLVMGetNextUse(u);
            }
        }
        users
    }

    // Calls.
    pub fn callee(&self, call: V) -> V {
        unsafe { LLVMGetCalledValue(call) }
    }
    pub fn args(&self, call: V) -> u32 {
        unsafe { LLVMGetNumArgOperands(call) }
    }
    /// The function a call names directly.
    pub fn called_function(&self, call: V) -> Option<V> {
        let callee = self.callee(call);
        self.is_function(callee).then_some(callee)
    }
    pub fn is_inline_asm(&self, call: V) -> bool {
        unsafe { !LLVMIsAInlineAsm(self.callee(call)).is_null() }
    }

    // Types and layout.
    pub fn is_aggregate(&self, v: V) -> bool {
        unsafe {
            matches!(
                LLVMGetTypeKind(LLVMTypeOf(v)),
                LLVMTypeKind::LLVMStructTypeKind | LLVMTypeKind::LLVMArrayTypeKind
            )
        }
    }
    pub fn size(&self, v: V) -> i64 {
        unsafe { LLVMABISizeOfType(self.td, LLVMTypeOf(v)) as i64 }
    }
    pub fn const_int(&self, v: V) -> Option<i64> {
        unsafe {
            if LLVMIsAConstantInt(v).is_null() || LLVMGetIntTypeWidth(LLVMTypeOf(v)) > 64 {
                None
            } else {
                Some(LLVMConstIntGetSExtValue(v))
            }
        }
    }
    /// The byte offset of field `index` of `ty`, and the field's type.
    fn field(&self, ty: LLVMTypeRef, index: u64) -> Option<(i64, LLVMTypeRef)> {
        unsafe {
            match LLVMGetTypeKind(ty) {
                LLVMTypeKind::LLVMStructTypeKind => Some((
                    LLVMOffsetOfElement(self.td, ty, index as u32) as i64,
                    LLVMStructGetTypeAtIndex(ty, index as u32),
                )),
                LLVMTypeKind::LLVMArrayTypeKind | LLVMTypeKind::LLVMVectorTypeKind => {
                    let element = LLVMGetElementType(ty);
                    Some(((index as i64).wrapping_mul(LLVMABISizeOfType(self.td, element) as i64), element))
                }
                _ => None,
            }
        }
    }
    /// The constant byte offset a GEP (instruction or expression) adds.
    pub fn gep_offset(&self, gep: V) -> i64 {
        unsafe {
            let mut ty = LLVMGetGEPSourceElementType(gep);
            let mut offset = 0i64;
            for (i, index) in self.operands(gep).into_iter().enumerate().skip(1) {
                let Some(index) = self.const_int(index) else { return ANY };
                if i == 1 {
                    offset = offset.wrapping_add(index.wrapping_mul(LLVMABISizeOfType(self.td, ty) as i64));
                } else {
                    let Some((delta, field)) = self.field(ty, index as u64) else { return ANY };
                    offset = offset.wrapping_add(delta);
                    ty = field;
                }
            }
            offset
        }
    }
    /// The byte offset of the field an extractvalue or insertvalue names.
    pub fn indices_offset(&self, inst: V, aggregate: V) -> i64 {
        unsafe {
            let mut ty = LLVMTypeOf(aggregate);
            let indices = std::slice::from_raw_parts(LLVMGetIndices(inst), LLVMGetNumIndices(inst) as usize);
            let mut offset = 0;
            for &index in indices {
                let Some((delta, field)) = self.field(ty, index as u64) else { return ANY };
                offset += delta;
                ty = field;
            }
            offset
        }
    }
    /// The byte offset of operand `index` of a constant aggregate.
    pub fn element_offset(&self, aggregate: V, index: u32) -> i64 {
        unsafe { self.field(LLVMTypeOf(aggregate), index as u64).map_or(ANY, |f| f.0) }
    }

    // Debug info.
    pub fn location(&self, inst: V) -> Md {
        unsafe { LLVMInstructionGetDebugLoc(inst) }
    }
    pub fn inlined_at(&self, location: Md) -> Md {
        unsafe { LLVMDILocationGetInlinedAt(location) }
    }
    fn metadata_operands(&self, md: Md) -> Vec<V> {
        unsafe {
            let value = LLVMMetadataAsValue(self.ctx, md);
            let mut operands = vec![std::ptr::null_mut(); LLVMGetMDNodeNumOperands(value) as usize];
            LLVMGetMDNodeOperands(value, operands.as_mut_ptr());
            operands
        }
    }
    /// The subprogram a location's scope is in.
    pub fn subprogram(&self, location: Md) -> Option<Rc<Sub>> {
        let mut scope = unsafe { LLVMDILocationGetScope(location) };
        if let Some(sub) = self.scopes.borrow().get(&scope) {
            return sub.clone();
        }
        let start = scope;
        let sub = unsafe {
            loop {
                if scope.is_null() {
                    break None;
                }
                match LLVMGetMetadataKind(scope) {
                    LLVMMetadataKind::LLVMDISubprogramMetadataKind => {
                        // Operands: file, scope, name, linkage name, ...
                        let operands = self.metadata_operands(scope);
                        let text = |i: usize| {
                            let value = *operands.get(i)?;
                            if value.is_null() {
                                return None;
                            }
                            let mut len = 0;
                            let ptr = LLVMGetMDString(value, &mut len);
                            (!ptr.is_null() && len > 0).then(|| {
                                String::from_utf8_lossy(std::slice::from_raw_parts(ptr.cast(), len as usize))
                                    .into_owned()
                            })
                        };
                        let name = demangle(&text(3).or_else(|| text(2)).unwrap_or_default());
                        break Some(Rc::new(Sub { channel: channel(&name) }));
                    }
                    LLVMMetadataKind::LLVMDILexicalBlockMetadataKind | LLVMMetadataKind::LLVMDILexicalBlockFileMetadataKind => {
                        let parent = self.metadata_operands(scope).get(1).copied().unwrap_or(std::ptr::null_mut());
                        scope = if parent.is_null() { std::ptr::null_mut() } else { LLVMValueAsMetadata(parent) };
                    }
                    _ => break None,
                }
            }
        };
        self.scopes.borrow_mut().insert(start, sub.clone());
        sub
    }
    /// The functions an instruction was inlined from, innermost first, each
    /// with the place it was inlined at. The last entry, with a null place, is
    /// the function the instruction is in.
    pub fn inline_chain(&self, inst: V) -> Vec<(Option<Rc<Sub>>, Md)> {
        let mut chain = vec![];
        let mut location = self.location(inst);
        while !location.is_null() {
            let at = self.inlined_at(location);
            chain.push((self.subprogram(location), at));
            location = at;
        }
        chain
    }
}

unsafe fn cstr(message: *mut libc_char) -> String {
    if message.is_null() {
        return String::new();
    }
    unsafe { std::ffi::CStr::from_ptr(message).to_string_lossy().into_owned() }
}

#[allow(non_camel_case_types)]
type libc_char = std::ffi::c_char;
