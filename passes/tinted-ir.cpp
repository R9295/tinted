// Context-sensitive value and callback analysis over linked LLVM IR.
// Validators are trusted contracts: instruction order and predicate correctness
// are deliberately outside this analysis.
#include "llvm/IR/Constants.h"
#include "llvm/IR/DataLayout.h"
#include "llvm/IR/DebugInfoMetadata.h"
#include "llvm/IR/DebugProgramInstruction.h"
#include "llvm/IR/InstIterator.h"
#include "llvm/IR/Instructions.h"
#include "llvm/IR/IntrinsicInst.h"
#include "llvm/IR/Module.h"
#include "llvm/IR/Operator.h"
#include "llvm/IR/IRBuilder.h"
#include "llvm/IRReader/IRReader.h"
#include "llvm/Demangle/Demangle.h"
#include "llvm/Support/SourceMgr.h"
#include "llvm/Support/raw_ostream.h"
#include <algorithm>
#include <deque>
#include <cstdlib>
#include <map>
#include <memory>
#include <set>
#include <string>
#include <tuple>
#include <vector>
using namespace llvm;
namespace {
constexpr int64_t AnyOffset = INT64_MIN;
struct Ref {
  unsigned object;
  int64_t offset;
  uint64_t tags;
  auto operator<=>(const Ref &) const = default;
};
using ValueSet = std::set<Ref>;
struct Context;
struct Object {
  const Value *value;
  Context *owner;
  std::map<int64_t, ValueSet> fields;
  std::set<Context *> readers;
};
struct Context {
  Function *fn;
  const Instruction *parentSite;
  const Instruction *site;
  uint64_t tags;
  std::map<const Value *, ValueSet> values;
  ValueSet result;
  std::set<Context *> callers;
  bool queued = false;
  // Set when only the code inlined from one function is analysed.
  const DISubprogram *bodySp = nullptr;
  const DILocation *bodyAt = nullptr;
};
// A summarized library call. It is either a real call instruction, or the
// instructions left behind where the optimizer inlined the library function
// into its caller (a Region, recognised from debug info).
struct Site {
  const Instruction *key = nullptr;
  const Value *value = nullptr;       // the call; null for an inlined region
  const void *identity = nullptr;     // names the abstract objects of a region
  std::string name;                   // demangled callee
  std::vector<std::string> argNames;
  bool sret = false;                  // argument 0 is the return slot
  uint64_t retSize = 0, sretSize = 0;
  bool region = false;
  ValueSet inputs;                    // region: every value flowing in
};
struct Region {
  const DILocation *at = nullptr;     // where the library function was inlined
  std::string name;
  const Instruction *anchor = nullptr;
  std::set<const Instruction *> members;
  std::vector<const Value *> inputs;          // defined outside, used inside
  std::vector<const Instruction *> outputs;   // defined inside, used outside
  std::vector<const Value *> written;         // outside memory written inside
  std::vector<std::string> argNames;
  std::vector<std::vector<const Value *>> argValues;
};
struct FunctionRegions {
  std::map<const Instruction *, Region *> of;
  std::vector<std::unique_ptr<Region>> all;
  // Validation tags of instructions inlined from a validate function.
  std::map<const Instruction *, uint64_t> validated;
  // With a body filter: the body's instructions and the values flowing in.
  std::set<const Instruction *> body;
  std::vector<const Value *> bodyInputs;
};
// A function body that exists only inlined into `host`.
struct Inlined {
  Function *host;
  const DISubprogram *sp;
  const DILocation *at;
};
enum class Effect { Value, Payload, None };
class Analysis {
  Module &module;
  const DataLayout &layout;
  // Object 0 is an unknown pointer; object 1 is a scalar. Scalar identity is
  // immaterial under the trusted-validation contract, so keep only its tags.
  std::vector<Object> objects = {{nullptr, nullptr, {}, {}}, {nullptr, nullptr, {}, {}}};
  std::map<std::pair<const void *, Context *>, unsigned> objectIds;
  std::map<std::pair<const Function *, const DILocation *>, FunctionRegions> regionCache;
  std::map<const DILocation *, uint64_t> inlinedValidators;
  std::map<std::string, std::vector<Inlined>> inlinedIndex;
  bool inlinedIndexed = false;
  std::map<const DISubprogram *, std::pair<std::string, bool>> subprograms;
  using Key = std::tuple<Function *, const Instruction *, const Instruction *, uint64_t, const DILocation *>;
  std::map<Key, std::unique_ptr<Context>> contexts;
  std::deque<Context *> work;
  std::map<Function *, uint64_t> validators, sinks;
  std::map<std::string, unsigned> labels;
  std::map<std::string, Function *> named;
  std::map<unsigned, ValueSet> messages;
  std::map<unsigned, std::set<Context *>> receivers;
  // These abstract records represent runtime internals whose user-visible
  // callbacks have already been expanded by a library summary.
  std::set<unsigned> opaqueObjects{0};
  std::set<std::pair<Context *, const CallBase *>> unresolved;
  size_t rounds = 0;
  bool exhausted = false;
  static bool merge(ValueSet &to, const ValueSet &from) {
    size_t before = to.size();
    to.insert(from.begin(), from.end());
    return before != to.size();
  }
  static bool contains(const std::string &name, const std::string &part) {
    return name.find(part) != std::string::npos;
  }
  static bool taskBoundary(const std::string &name) {
    if (name.starts_with("<rayon::") && contains(name,"::for_each::<")) return true;
    if (name.starts_with("<tokio::runtime::builder::Builder>") && contains(name,"::on_thread_") && contains(name,"::<")) return true;
    if (name.starts_with("std::thread::spawnhook::add_spawn_hook::<")) return true;
    bool runtime = name.starts_with("tokio::") || name.starts_with("<tokio::") ||
      name.starts_with("std::thread::") || name.starts_with("<std::thread::");
    return runtime && (contains(name,"::spawn::<") || contains(name,"::spawn_local::<") ||
      contains(name,"::spawn_blocking::<") || contains(name,"::block_on::<"));
  }
  // The public channel API of std::sync::mpsc and crossbeam_channel. The
  // crossbeam `select!` macro receives through SelectedOperation or `read`.
  static bool crossbeam(const std::string &name) {
    return name.starts_with("crossbeam_channel::channel::") || name.starts_with("<crossbeam_channel::channel::") ||
      name.starts_with("<crossbeam_channel::select::SelectedOperation>::");
  }
  static bool channelCreate(const std::string &name) {
    return contains(name,"::channel::<") || contains(name,"::sync_channel::<") ||
      (crossbeam(name) && (contains(name,"::unbounded::<") || contains(name,"::bounded::<")));
  }
  static bool channelSend(const std::string &name) {
    return contains(name,"::send") || contains(name,"::try_send") || (crossbeam(name) && contains(name,"::write::<"));
  }
  static bool channelBoundary(const std::string &name) {
    bool mpsc=name.starts_with("std::sync::mpsc::") || name.starts_with("<std::sync::mpsc::");
    if (!mpsc && !crossbeam(name)) return false;
    return channelCreate(name) || channelSend(name) || contains(name,"::recv") || contains(name,"::try_recv") ||
      name.ends_with("::next") || (!mpsc && contains(name,"::read::<"));
  }
  // Waiting on several channels is opaque; the receive that follows it is a
  // channel operation of its own.
  static bool selectBoundary(const std::string &name) {
    return name.starts_with("crossbeam_channel::select::") || name.starts_with("<crossbeam_channel::select::Select");
  }
  static bool runtimeBoundary(const std::string &name) {
    if (name.starts_with("<tokio::runtime::task::error::JoinError as core::fmt::")) return true;
    for (auto type : {"tokio::runtime::builder::Builder", "tokio::runtime::runtime::Runtime",
         "tokio::runtime::task::join::JoinHandle<", "tokio::runtime::context::enter::EnterGuard",
         "tokio::runtime::context::current::SetCurrentGuard", "tokio::runtime::handle::Handle",
         "tokio::task::local::LocalSet"})
      if (name.starts_with(std::string("<")+type) || name.starts_with(std::string("core::ptr::drop_glue::<")+type)) return true;
    return name=="core::ptr::drop_glue::<tokio::runtime::context::Context>" ||
      (name.starts_with("std::sys::thread_local::") && contains(name,"::destroy::<tokio::runtime::context::Context>"));
  }
  static std::string firstType(const std::string &name) {
    auto begin = name.rfind("::<");
    if (begin == std::string::npos) return {};
    begin += 3;
    int nesting = 0;
    for (size_t i=begin; i<name.size(); ++i) {
      char c=name[i];
      if (c=='<' || c=='(' || c=='[') ++nesting;
      if ((c==',' || c=='>') && nesting==0) return name.substr(begin,i-begin);
      if (c=='>' || c==')' || c==']') --nesting;
    }
    return {};
  }
  static ValueSet tagged(ValueSet values, uint64_t tags) {
    if (!tags) return values;
    ValueSet result;
    for (auto ref : values) { ref.tags |= tags; result.insert(ref); }
    return result;
  }
  void schedule(Context *ctx) {
    if (!ctx->queued) { ctx->queued = true; work.push_back(ctx); }
  }
  unsigned object(const Value *value, Context *ctx = nullptr) {
    auto key = std::make_pair(static_cast<const void *>(value), ctx);
    if (auto it = objectIds.find(key); it != objectIds.end()) return it->second;
    unsigned id = objects.size();
    objects.push_back({value, ctx, {}, {}});
    objectIds[key] = id;
    if (auto *global = dyn_cast<GlobalVariable>(value))
      if (global->hasInitializer()) initialize(id, 0, global->getInitializer());
    if (auto *constant=dyn_cast<Constant>(value); constant && constant->getType()->isAggregateType() && !isa<GlobalValue>(value))
      initialize(id,0,constant);
    return id;
  }
  // An object that stands for no IR value: the token, message or handle that
  // a summary of an inlined library call creates.
  unsigned abstractObject(const void *identity, Context *ctx) {
    auto key = std::make_pair(identity, ctx);
    if (auto it = objectIds.find(key); it != objectIds.end()) return it->second;
    unsigned id = objects.size();
    objects.push_back({nullptr, ctx, {}, {}});
    objectIds[key] = id;
    return id;
  }
  unsigned siteObject(const Site &site, Context *ctx) {
    return site.region ? abstractObject(site.identity, ctx) : object(site.value, ctx);
  }
  ValueSet reference(const Value *value, Context *ctx = nullptr) {
    if (!value->getType()->isPointerTy() && !value->getType()->isAggregateType())
      return {{1, 0, ctx ? ctx->tags : 0}};
    return {{object(value, ctx), 0, ctx ? ctx->tags : 0}};
  }
  void initialize(unsigned id, int64_t off, const Constant *c) {
    if (isa<ConstantAggregateZero>(c)) {
      uint64_t size=layout.getTypeAllocSize(c->getType()).getFixedValue();
      auto zero=reference(ConstantPointerNull::get(PointerType::get(module.getContext(),0)));
      for (uint64_t i=0;i<size && i<=256;i+=layout.getPointerSize()) merge(objects[id].fields[off+i],zero);
      if (size>256) merge(objects[id].fields[AnyOffset],zero);
    } else if (auto *structure = dyn_cast<ConstantStruct>(c)) {
      auto *sl = layout.getStructLayout(structure->getType());
      for (unsigned i=0; i<c->getNumOperands(); ++i)
        initialize(id, off + sl->getElementOffset(i), cast<Constant>(c->getOperand(i)));
    } else if (auto *array = dyn_cast<ConstantArray>(c)) {
      auto stride = layout.getTypeAllocSize(array->getType()->getElementType()).getFixedValue();
      for (unsigned i=0; i<c->getNumOperands(); ++i)
        initialize(id, off + i*stride, cast<Constant>(c->getOperand(i)));
    } else if (auto *bytes=dyn_cast<ConstantDataArray>(c); bytes && bytes->getElementType()->isIntegerTy(8)) {
      // Rust packs vtable metadata (including a null drop slot) into byte
      // arrays. Preserve zero pointer words rather than inventing callbacks.
      unsigned width=layout.getPointerSize();
      for (unsigned i=0;i<bytes->getNumElements();i+=width) {
        bool zero=i+width<=bytes->getNumElements();
        for (uint64_t j=i;j<std::min<uint64_t>(i+width,bytes->getNumElements());++j)
          zero &= bytes->getElementAsInteger(j)==0;
        auto values=zero ? reference(ConstantPointerNull::get(PointerType::get(module.getContext(),0))) : ValueSet{{1,0,0}};
        merge(objects[id].fields[off+i],values);
      }
    } else {
      auto values = eval(c, nullptr);
      merge(objects[id].fields[off], values);
    }
  }
  ValueSet offset(ValueSet base, int64_t delta) {
    ValueSet result;
    for (auto ref : base) {
      ref.offset = ref.offset == AnyOffset || delta == AnyOffset ? AnyOffset : ref.offset + delta;
      // Widen array/iterator offsets so pointer-arithmetic cycles have a
      // finite domain. Concrete struct fields remain distinct.
      if (ref.offset < 0 || ref.offset > 256) ref.offset = AnyOffset;
      result.insert(ref);
    }
    return result;
  }
  int64_t gepOffset(const GEPOperator *gep) {
    APInt off(layout.getIndexTypeSizeInBits(gep->getType()), 0);
    return gep->accumulateConstantOffset(layout, off) ? off.getSExtValue() : AnyOffset;
  }
  int64_t aggregateOffset(Type *type, ArrayRef<unsigned> indices) {
    int64_t off = 0;
    for (auto index : indices) {
      if (auto *st = dyn_cast<StructType>(type)) {
        off += layout.getStructLayout(st)->getElementOffset(index);
        type = st->getElementType(index);
      } else if (auto *array = dyn_cast<ArrayType>(type)) {
        type = array->getElementType();
        off += index * layout.getTypeAllocSize(type).getFixedValue();
      }
    }
    return off;
  }
  ValueSet eval(const Value *value, Context *ctx) {
    if (auto *integer=dyn_cast<ConstantInt>(value); integer && integer->isZero())
      return tagged(reference(ConstantPointerNull::get(PointerType::get(module.getContext(),0))),ctx ? ctx->tags : 0);
    if (auto *alias = dyn_cast<GlobalAlias>(value)) return eval(alias->getAliasee(), ctx);
    if (isa<GlobalValue>(value)) return tagged(reference(value), ctx ? ctx->tags : 0);
    if (auto *expr = dyn_cast<ConstantExpr>(value)) {
      if (auto *gep = dyn_cast<GEPOperator>(expr))
        return offset(eval(gep->getPointerOperand(), ctx), gepOffset(gep));
      if (expr->isCast()) return eval(expr->getOperand(0), ctx);
    }
    if (isa<Constant>(value)) return tagged(reference(value), ctx ? ctx->tags : 0);
    if (!ctx) return {};
    return tagged(ctx->values[value], ctx->tags);
  }
  ValueSet load(const ValueSet &pointers, Context *ctx) {
    ValueSet result;
    for (auto ptr : pointers) {
      objects[ptr.object].readers.insert(ctx);
      bool found=false;
      for (const auto &[off, values] : objects[ptr.object].fields)
        if (off == ptr.offset || off == AnyOffset || ptr.offset == AnyOffset) {
          merge(result, tagged(values, ptr.tags));
          found=true;
        }
      if (!found && (opaqueObjects.contains(ptr.object) ||
          isa_and_nonnull<ConstantPointerNull>(objects[ptr.object].value))) result.insert(ptr);
    }
    return result;
  }
  void store(const ValueSet &pointers, const ValueSet &values) {
    for (auto ptr : pointers) {
      if (ptr.object < 2 || isa_and_nonnull<ConstantPointerNull>(objects[ptr.object].value)) continue;
      if (auto *global=dyn_cast_or_null<GlobalVariable>(objects[ptr.object].value); global && global->isConstant()) continue;
      if (merge(objects[ptr.object].fields[ptr.offset], values)) {
        auto readers = objects[ptr.object].readers;
        for (auto *ctx : readers) if (ctx) schedule(ctx);
      }
    }
  }
  void copy(const ValueSet &to, const ValueSet &from, Context *ctx, uint64_t size = UINT64_MAX) {
    for (auto src : from) {
      objects[src.object].readers.insert(ctx);
      auto fields = objects[src.object].fields;
      for (auto &[off, values] : fields) {
        int64_t delta = off == AnyOffset || src.offset == AnyOffset ? AnyOffset : off-src.offset;
        if (delta != AnyOffset && (delta < 0 || uint64_t(delta) >= size)) continue;
        store(offset(to, delta), tagged(values, src.tags | ctx->tags));
      }
    }
  }
  Context *invoke(Function *fn, const Instruction *site, Context *caller,
                  const std::vector<ValueSet> &args, uint64_t tags, const Inlined *inlined = nullptr) {
    // A host of an inlined body is not validated by its own annotation; a
    // marker inside the body tags the body's instructions instead.
    if (!inlined) tags |= validators[fn];
    Key key{fn, caller ? caller->site : nullptr, site, tags, inlined ? inlined->at : nullptr};
    auto it = contexts.find(key);
    Context *ctx;
    if (it == contexts.end()) {
      if (contexts.size() >= 50000) { exhausted = true; return nullptr; }
      auto fresh = std::make_unique<Context>();
      ctx = fresh.get(); ctx->fn = fn;
      ctx->parentSite = caller ? caller->site : nullptr; ctx->site = site; ctx->tags = tags;
      if (inlined) { ctx->bodySp = inlined->sp; ctx->bodyAt = inlined->at; }
      contexts.emplace(key, std::move(fresh)); schedule(ctx);
    } else ctx = it->second.get();
    if (caller) ctx->callers.insert(caller);
    for (unsigned i=0; i<args.size() && i<fn->arg_size(); ++i)
      if (merge(ctx->values[fn->getArg(i)], tagged(args[i], tags))) schedule(ctx);
    return ctx;
  }
  // Foreign/prebuilt code can invoke callbacks it receives. Traverse the
  // passed objects rather than guessing targets from an ABI-wide type ID.
  void externalCallbacks(const Instruction *key, const std::string &calleeName, Context *ctx,
                         const std::vector<ValueSet> &args) {
    // A fixpoint over references: each one carries the union of the
    // receivers it was reached with and is revisited only when that union
    // grows. Visiting it once per distinct receiver set does not terminate
    // on large object graphs.
    std::deque<Ref> pending;
    std::set<Ref> queued, visited;
    std::map<Ref, ValueSet> seen;
    auto reach = [&](Ref ref, const ValueSet &receiver) {
      if (ref.object < 2) return;
      bool grew = merge(seen[ref], receiver);
      if ((grew || !visited.contains(ref)) && queued.insert(ref).second) pending.push_back(ref);
    };
    bool registration=calleeName.starts_with("std::sys::thread_local::destructors::") && calleeName.ends_with("::register");
    for (unsigned i=registration ? 1 : 0; i<args.size(); ++i)
      for (auto ref : args[i]) reach(ref, i ? args[i-1] : ValueSet{});
    while (!pending.empty()) {
      auto ref = pending.front(); pending.pop_front();
      queued.erase(ref); visited.insert(ref);
      ValueSet receiver = seen[ref];
        auto *fn = dyn_cast_or_null<Function>(objects[ref.object].value);
        if (fn && !fn->isDeclaration() && !fn->getName().contains("__sink__") &&
            !fn->getName().contains("__validate__") && !fn->getName().contains("__source__")) {
          if (runtimeBoundary(llvm::demangle(fn->getName().str()))) continue;
          std::vector<ValueSet> callbackArgs;
          for (auto &arg : fn->args())
            callbackArgs.push_back(arg.getType()->isPointerTy() ? receiver : ValueSet{{1,0,ctx->tags|ref.tags}});
          invoke(const_cast<Function *>(fn), key, ctx, callbackArgs, ctx->tags | ref.tags);
        }
        if (registration) continue;
        objects[ref.object].readers.insert(ctx);
        auto fields = objects[ref.object].fields;
        for (const auto &[off, values] : fields) {
          ValueSet payload = receiver;
          ValueSet adjacent;
          for (int direction : {-1,1}) {
            if (off==AnyOffset) continue;
            auto neighbor=fields.find(off+direction*int64_t(layout.getPointerSize()));
            if (neighbor==fields.end()) continue;
            for (auto data:neighbor->second)
              if (data.object>=2 && !isa_and_nonnull<Function>(objects[data.object].value)) adjacent.insert(data);
          }
          if (!adjacent.empty()) payload=tagged(adjacent,ref.tags);
          // The data and vtable/function fields of Rust's callback records
          // travel together. Do not feed the entire transitive object graph
          // back as every callback parameter.
          if (payload.empty()) payload.insert(ref);
          for (auto child : tagged(values, ref.tags)) reach(child, payload);
        }
      }
  }
  ValueSet opaque(const Site &site, Context *ctx, const std::vector<ValueSet> &args) {
    unsigned id=siteObject(site,ctx);
    opaqueObjects.insert(id);
    ValueSet value{{id,0,ctx->tags}};
    for (uint64_t off=0; off<site.retSize; off+=8) store(offset(value,off), value);
    if (site.sret && !args.empty())
      for (uint64_t off=0; off<site.sretSize; off+=8) store(offset(args[0],off), value);
    return value;
  }
  // Every place a function was inlined, by the function's name.
  const std::vector<Inlined> inlinedBodies(const std::string &type, bool poll) {
    if (!inlinedIndexed) {
      inlinedIndexed = true;
      std::set<const DILocation *> seen;
      for (auto &fn : module) for (auto &inst : instructions(fn))
        for (const DILocation *loc = inst.getDebugLoc().get(); loc && loc->getInlinedAt(); loc = loc->getInlinedAt()) {
          if (!seen.insert(loc->getInlinedAt()).second) break;
          if (auto *sp = loc->getScope()->getSubprogram())
            inlinedIndex[subprogram(sp).first].push_back({&fn, sp, loc->getInlinedAt()});
        }
    }
    std::vector<Inlined> result;
    if (auto it = inlinedIndex.find(type); it != inlinedIndex.end()) result = it->second;
    if (poll) {
      auto prefix = "<"+type+" as ";
      for (auto it = inlinedIndex.lower_bound(prefix); it != inlinedIndex.end() && it->first.starts_with(prefix); ++it)
        if (it->first.ends_with("::poll")) result.insert(result.end(), it->second.begin(), it->second.end());
    }
    return result;
  }
  // Analyses a body that was inlined into `place.host`: only its own
  // instructions, in a context of their own. Values flowing into them come
  // from the caller when the body sits in the caller's function, and
  // otherwise from the environment the task was submitted with.
  void invokeInlined(const Inlined &place, const Instruction *site, Context *caller, const ValueSet &env) {
    auto *ctx = invoke(place.host, site, caller, {}, caller->tags, &place);
    if (!ctx) return;
    auto &regions = regionsOf(place.host, place.sp, place.at);
    for (auto *input : regions.bodyInputs) {
      ValueSet values;
      if (place.host == caller->fn) values = eval(input, caller);
      else if (input->getType()->isPointerTy()) {
        values = offset(env, AnyOffset);
        merge(values, load(values, caller));
      } else values = {{1,0,caller->tags}};
      if (std::getenv("TINTED_TRACE")) {
        errs()<<"body-input "<<llvm::demangle(place.host->getName().str()).substr(0,60)<<" "; input->printAsOperand(errs(),false); errs()<<" =";
        for (auto ref : values) errs()<<" "<<ref.object<<":"<<ref.offset<<":"<<ref.tags<<(messages.contains(ref.object)?"(chan)":"");
        errs()<<"\n";
      }
      if (merge(ctx->values[input], tagged(values, caller->tags))) schedule(ctx);
    }
  }
  static bool channelDrop(const std::string &name) {
    for (auto lib : {"std::sync::mpsc::", "crossbeam_channel::channel::"})
      if (contains(name,std::string("core::ptr::drop_glue::<")+lib) ||
          (contains(name,lib) && contains(name,"core::ops::drop::Drop"))) return true;
    return false;
  }
  // Library functions with a summary. Applies to a call of one and to the
  // code left where one was inlined.
  static bool summarized(const std::string &name) {
    return channelBoundary(name) || channelDrop(name) || selectBoundary(name) || taskBoundary(name) || runtimeBoundary(name);
  }
  bool library(const Site &site, Context *ctx, const std::vector<ValueSet> &args,
               ValueSet &result, Effect &effect) {
    const std::string &name=site.name;
    auto argName=[&](unsigned i) { return i<site.argNames.size() ? StringRef(site.argNames[i]) : StringRef(); };
    if (name.starts_with("std::rt::lang_start::<") && !args.empty()) {
      for (auto ref:args[0])
        if (auto *entry=dyn_cast_or_null<Function>(objects[ref.object].value))
          invoke(const_cast<Function *>(entry),site.key,ctx,{},ctx->tags|ref.tags);
      result={{1,0,ctx->tags}};
      return true;
    }
    if (channelBoundary(name)) {
      if (channelCreate(name)) {
        unsigned token=siteObject(site,ctx);
        messages.try_emplace(token);
        ValueSet handle{{token,0,ctx->tags}};
        result=handle;
        if (!args.empty() && site.sret)
          for (unsigned off=0;off<32;off+=8) store(offset(args[0],off),handle);
        return true;
      }
      bool sending=channelSend(name);
      unsigned self=site.sret ? 1 : 0;
      if (!site.region && args.size()<=self) return false;
      // The channel is found from the handle the call is made on. `select!`
      // passes the handle as a later argument, and where the call was inlined
      // `self` may be gone; the handle is then among the values flowing into
      // the inlined code.
      std::set<unsigned> tokens;
      ValueSet everything;
      for (auto &arg : args) merge(everything, arg);
      const ValueSet *self_=args.size()>self ? &args[self] : nullptr, *all_=&everything, *inputs_=&site.inputs;
      for (auto *from : {self_, all_, inputs_}) {
        if (!from || !tokens.empty()) continue;
        std::set<unsigned> visited;
        ValueSet pending=*from;
        while (!pending.empty()) {
          auto ref=*pending.begin(); pending.erase(pending.begin());
          if (!visited.insert(ref.object).second) continue;
          if (messages.contains(ref.object)) { tokens.insert(ref.object); continue; }
          objects[ref.object].readers.insert(ctx);
          for (auto &[_,values]:objects[ref.object].fields) merge(pending,values);
        }
      }
      result={{siteObject(site,ctx),0,ctx->tags}};
      if (sending) {
        effect=Effect::None;
        bool payload=false;
        unsigned slot=0;
        for (unsigned i=self+1;i<args.size();++i) {
          auto an=argName(i);
          if (an=="token" || an=="deadline" || an=="timeout") continue;
          store(offset(result,slot++*8),args[i]);
          payload |= !args[i].empty();
        }
        // The optimizer drops a message argument that is the same constant at
        // every call, or that it can no longer describe. Whatever is sent was
        // still computed in this context.
        if (!payload) store(result, site.inputs.empty() ? ValueSet{{1,0,ctx->tags}} : site.inputs);
        for (auto token:tokens)
          if (merge(messages[token],result)) for (auto *reader:receivers[token]) schedule(reader);
      } else {
        effect=Effect::Payload;
        for (auto token:tokens) {
          receivers[token].insert(ctx);
          // Result/Option may use an explicit discriminant or a pointer
          // niche. Retain the payload in either ABI position.
          copy(result,messages[token],ctx);
          copy(offset(result,8),messages[token],ctx);
          if (self) { copy(args[0],messages[token],ctx); copy(offset(args[0],8),messages[token],ctx); }
        }
      }
      return true;
    }
    if (channelDrop(name)) {
      effect=Effect::None;
      result=opaque(site,ctx,args);
      return true;
    }
    if (selectBoundary(name)) {
      result=opaque(site,ctx,args);
      return true;
    }
    if (taskBoundary(name)) {
      std::string type=firstType(name);
      bool future=contains(name,"tokio::") && !contains(name,"::spawn_blocking::<");
      bool lifecycle=contains(name,"::on_thread_");
      if (lifecycle) future=false;
      Function *body = named.contains(type) ? named[type] : nullptr;
      if (!body) for (auto &[pretty,candidate] : named)
        if (pretty.starts_with("<"+type+" as ") && pretty.ends_with("::poll")) { body=candidate; break; }
      // The optimizer may have left the task's body only as code inlined into
      // the runtime's poll or call wrappers.
      std::vector<Inlined> inlined;
      if (!body || body->isDeclaration()) {
        body=nullptr;
        inlined=inlinedBodies(type,true);
      }
      // A task's destructor runs in the runtime as well. It can be all that
      // is left of a task whose body was optimized to nothing.
      Function *dropBody=nullptr;
      std::vector<Inlined> inlinedDrop;
      for (auto dropName : {"core::ptr::drop_glue::<"+type+">", "<"+type+" as core::ops::drop::Drop>::drop"}) {
        if (auto it=named.find(dropName); it!=named.end() && !it->second->isDeclaration()) { dropBody=it->second; break; }
        inlinedDrop=inlinedBodies(dropName,false);
        if (!inlinedDrop.empty()) break;
      }
      if (!body && inlined.empty() && !dropBody && inlinedDrop.empty()) return false;
      std::vector<ValueSet> payload;
      for (unsigned i=0; i<args.size(); ++i) {
        auto an=argName(i);
        if ((an=="future" || an.starts_with("future.") || an=="f" || an.starts_with("f.") || an=="task") &&
            !(site.region && args[i].empty()))
          payload.push_back(args[i]);
      }
      if (payload.empty()) for (unsigned i=0; i<args.size(); ++i) {
        auto an=argName(i);
        if (!(site.sret && i==0) && !an.starts_with("self") && an!="rt" && an!="runtime" &&
            !(site.region && args[i].empty())) payload.push_back(args[i]);
      }
      if (payload.empty() && site.region && !site.inputs.empty()) payload.push_back(site.inputs);
      ValueSet env;
      if (payload.size()==1 && !payload[0].empty()) {
        // The payload is the task's environment when it points at memory.
        // A payload reduced to the captured values themselves (a handle, a
        // zero) is stored in an environment record instead.
        bool direct=false;
        for (auto ref:payload[0]) {
          auto *value=ref.object>=2 ? objects[ref.object].value : nullptr;
          if (value && isa<ConstantPointerNull>(value)) continue;
          if (value && value->getType()->isPointerTy()) env.insert(ref);
          else if (ref.object>=2) direct=true;
        }
        if (direct) env.clear();
      }
      if (env.empty()) {
        env={{siteObject(site,ctx),0,ctx->tags}};
        for (unsigned i=0; i<payload.size(); ++i) store(offset(env,i*8),payload[i]);
      }
      std::vector<ValueSet> input(body ? body->arg_size() : 0, {{1,0,ctx->tags}});
      for (unsigned i=0; i<input.size(); ++i) {
        if (future) input[i]=i==0 ? env : ValueSet{{0,0,ctx->tags}};
        else if (i<payload.size()) input[i]=payload[i];
        if (i==0 && body->getArg(i)->getType()->isPointerTy()) input[i]=env;
      }
      if (contains(name,"rayon::") && input.size()>1 && !args.empty()) input[1]=args[0];
      if (body && !runtimeBoundary(llvm::demangle(body->getName().str())))
        invoke(body,site.key,ctx,input,ctx->tags);
      for (auto &place : inlined) invokeInlined(place,site.key,ctx,env);
      if (body && name.starts_with("std::thread::spawnhook::add_spawn_hook::<")) {
        // This API returns a second callback from the parent hook; the
        // runtime runs that callback in the new thread.
        auto split=name.find(", ",name.find("::<")+3);
        if (split!=std::string::npos && name.ends_with(">")) {
          auto childType=name.substr(split+2,name.size()-split-3);
          if (auto it=named.find(childType);it!=named.end()) {
            auto *parent=invoke(body,site.key,ctx,input,ctx->tags);
            std::vector<ValueSet> childArgs(it->second->arg_size(),parent ? parent->result : ValueSet{});
            invoke(it->second,site.key,ctx,childArgs,ctx->tags);
          }
        }
      }
      if (dropBody && !runtimeBoundary(llvm::demangle(dropBody->getName().str())))
        invoke(dropBody,site.key,ctx,{env},ctx->tags);
      for (auto &place : inlinedDrop) invokeInlined(place,site.key,ctx,env);
      effect=Effect::None;
      result=lifecycle && !args.empty() ? args[0] : opaque(site,ctx,args);
      return true;
    }
    // Runtime management is opaque after submission/poll/drop have been
    // summarized. Builder callbacks remain observable through their values.
    if (runtimeBoundary(name)) {
      if (contains(name,"Builder") && contains(name,">::build")) externalCallbacks(site.key,name,ctx,args);
      // Let builder setters populate the actual callback fields.
      if (name.starts_with("<tokio::runtime::builder::Builder>") && !contains(name,">::build") && !contains(name,">::new_")) return false;
      if (contains(name,"::block_on")) return false;
      result=opaque(site,ctx,args);
      return true;
    }
    return false;
  }
  void call(CallBase &call, Context *ctx) {
    std::vector<ValueSet> args;
    for (auto &arg : call.args()) args.push_back(eval(arg, ctx));
    if (auto *mem = dyn_cast<MemTransferInst>(&call)) {
      auto *len = dyn_cast<ConstantInt>(mem->getLength());
      copy(eval(mem->getDest(), ctx), eval(mem->getSource(), ctx), ctx,
           len ? len->getZExtValue() : UINT64_MAX);
      return;
    }
    if (call.isInlineAsm()) return;
    if (auto *intrinsic = dyn_cast<IntrinsicInst>(&call)) {
      if (intrinsic->getIntrinsicID() == Intrinsic::threadlocal_address && !args.empty())
        if (merge(ctx->values[&call], args[0])) schedule(ctx);
      return;
    }
    ValueSet result;
    bool resolved = false;
    auto targets=eval(call.getCalledOperand(),ctx);
    bool modeled=!targets.empty();
    for (auto target : targets) {
      modeled &= opaqueObjects.contains(target.object) ||
        isa_and_nonnull<ConstantPointerNull>(objects[target.object].value);
      auto *fn = dyn_cast_or_null<Function>(objects[target.object].value);
      if (!fn) continue;
      resolved = true;
      if (fn->isIntrinsic()) continue;
      std::string name = llvm::demangle(fn->getName().str());
      if (fn->getName().contains("__source__") || fn->getName().contains("__validate__") ||
          fn->getName().contains("__sink__")) continue;
      ValueSet summary;
      Site site; site.key=&call; site.value=&call; site.name=name;
      for (auto &arg : fn->args()) site.argNames.push_back(arg.getName().str());
      site.sret=call.arg_size() && call.paramHasAttr(0,Attribute::StructRet);
      if (call.getType()->isAggregateType()) site.retSize=layout.getTypeAllocSize(call.getType()).getFixedValue();
      if (site.sret) site.sretSize=layout.getTypeAllocSize(call.getParamStructRetType(0)).getFixedValue();
      Effect effect=Effect::Value;
      if (library(site,ctx,args,summary,effect)) {
        merge(result,summary);
        continue;
      }
      if (fn->isDeclaration()) {
        if (name.find("__rust_alloc") != std::string::npos ||
            name.find("__rust_realloc") != std::string::npos || name == "malloc" || name == "calloc") {
          merge(result, reference(&call, ctx));
          if (name.find("realloc") != std::string::npos && !args.empty()) copy(result, args[0], ctx);
        } else {
          bool leaf = name.find("::panicking::") != std::string::npos;
          for (auto type : {"u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "bool", "char", "f32", "f64"})
            if (name.starts_with(std::string("<")+type+" as core::fmt::")) leaf = true;
          if (!leaf) externalCallbacks(&call, call.getCalledFunction() ? name : std::string(), ctx, args);
          if (!call.getType()->isVoidTy()) merge(result, reference(&call, ctx));
        }
      } else {
        auto *child = invoke(const_cast<Function *>(fn), &call, ctx, args, ctx->tags | target.tags);
        if (child) merge(result, child->result);
      }
    }
    auto key = std::make_pair(ctx, &call);
    if (!resolved && modeled) {
      Site site; site.key=&call; site.value=&call;
      site.sret=call.arg_size() && call.paramHasAttr(0,Attribute::StructRet);
      if (call.getType()->isAggregateType()) site.retSize=layout.getTypeAllocSize(call.getType()).getFixedValue();
      if (site.sret) site.sretSize=layout.getTypeAllocSize(call.getParamStructRetType(0)).getFixedValue();
      merge(result,opaque(site,ctx,args));
    }
    if (!resolved && !modeled) unresolved.insert(key); else unresolved.erase(key);
    if (merge(ctx->values[&call], result)) schedule(ctx);
  }
  const std::pair<std::string, bool> &subprogram(const DISubprogram *sp) {
    auto [it, fresh] = subprograms.try_emplace(sp);
    if (fresh) {
      it->second.first = llvm::demangle(sp->getLinkageName().str());
      it->second.second = summarized(it->second.first);
    }
    return it->second;
  }
  // Finds the code that inlining a summarized library function left in `fn`.
  // An instruction belongs to the outermost such function on its inlined-at
  // chain; one region per place the function was inlined.
  FunctionRegions &regionsOf(const Function *fn, const DISubprogram *bodySp = nullptr,
                             const DILocation *bodyAt = nullptr) {
    auto [cached, fresh] = regionCache.try_emplace(std::make_pair(fn, bodyAt));
    auto &regions = cached->second;
    if (!fresh) return regions;
    std::map<const DILocation *, Region *> byPlace;
    for (auto &inst : instructions(*fn)) {
      if (isa<AllocaInst>(inst)) continue;
      const DILocation *at = nullptr; const DISubprogram *callee = nullptr;
      bool inBody = !bodyAt; uint64_t validation = 0;
      for (const DILocation *loc = inst.getDebugLoc().get(); loc && loc->getInlinedAt(); loc = loc->getInlinedAt()) {
        if (auto found = inlinedValidators.find(loc->getInlinedAt()); found != inlinedValidators.end())
          validation |= found->second;
        auto *sp = loc->getScope()->getSubprogram();
        // Only what was inlined into the body itself counts for a body.
        if (bodyAt && loc->getInlinedAt() == bodyAt && sp == bodySp) { inBody = true; break; }
        if (sp && subprogram(sp).second) { at = loc->getInlinedAt(); callee = sp; }
      }
      // Instructions the optimizer created (phis, hoisted address
      // computations) carry no location; they may belong to the body.
      if (bodyAt && !inst.getDebugLoc()) inBody = true;
      if (!inBody) continue;
      if (bodyAt) regions.body.insert(&inst);
      if (validation) regions.validated[&inst] = validation;
      if (!at) continue;
      auto &region = byPlace[at];
      if (!region) {
        regions.all.push_back(std::make_unique<Region>());
        region = regions.all.back().get();
        region->at = at; region->name = subprogram(callee).first; region->anchor = &inst;
      }
      region->members.insert(&inst);
      regions.of[&inst] = region;
    }
    // Instructions the optimizer created (phis, merged values) carry no
    // location. One computed only from an inlined call's instructions is
    // part of that inlined call.
    for (bool changed = !regions.all.empty(); changed;) {
      changed = false;
      for (auto &inst : instructions(*fn)) {
        if (inst.getDebugLoc() || isa<AllocaInst>(inst) || regions.of.contains(&inst)) continue;
        Region *region = nullptr; bool same = true;
        for (auto &operand : inst.operands()) {
          if (isa<Argument>(operand)) { same = false; break; }
          auto *other = dyn_cast<Instruction>(operand);
          if (!other) continue;
          auto found = regions.of.find(other);
          if (found == regions.of.end() || (region && region != found->second)) { same = false; break; }
          region = found->second;
        }
        if (!same || !region) continue;
        region->members.insert(&inst);
        regions.of[&inst] = region;
        changed = true;
      }
    }
    if (bodyAt) {
      std::set<const Value *> inputs;
      for (auto *inst : regions.body)
        for (auto &operand : inst->operands()) {
          auto *other = dyn_cast<Instruction>(operand);
          if ((isa<Argument>(operand) || (other && !isa<AllocaInst>(other) && !regions.body.contains(other))) && inputs.insert(operand).second)
            regions.bodyInputs.push_back(operand);
        }
    }
    if (regions.all.empty()) return regions;
    // The inlined function's arguments, as far as debug info still has them.
    for (auto &inst : instructions(*fn))
      for (const DbgVariableRecord &record : filterDbgVars(inst.getDbgRecordRange())) {
        auto *variable = record.getVariable();
        auto *loc = record.getDebugLoc().get();
        if (!variable || !variable->getArg() || !loc || !loc->getInlinedAt()) continue;
        auto found = byPlace.find(loc->getInlinedAt());
        if (found == byPlace.end()) continue;
        auto *region = found->second;
        auto *sp = variable->getScope()->getSubprogram();
        if (!sp || subprogram(sp).first != region->name) continue;
        unsigned index = variable->getArg()-1;
        if (region->argNames.size() <= index) { region->argNames.resize(index+1); region->argValues.resize(index+1); }
        region->argNames[index] = variable->getName().str();
        for (auto *value : record.location_ops())
          if (value && !isa<UndefValue>(value) && !isa<MetadataAsValue>(value)) region->argValues[index].push_back(value);
      }
    for (auto &owned : regions.all) {
      auto &region = *owned;
      std::set<const Value *> inputs, written;
      auto outside = [&](const Value *value) {
        if (isa<Argument>(value)) return true;
        auto *inst = dyn_cast<Instruction>(value);
        return inst && !region.members.contains(inst);
      };
      auto base = [&](const Value *value) {
        while (auto *inst = dyn_cast<Instruction>(value)) {
          if (!region.members.contains(inst) || !(isa<GetElementPtrInst>(inst) || isa<CastInst>(inst))) break;
          value = inst->getOperand(0);
        }
        return value;
      };
      for (auto *inst : region.members) {
        for (auto &operand : inst->operands())
          if (outside(operand) && inputs.insert(operand).second) region.inputs.push_back(operand);
        for (auto *user : inst->users())
          if (auto *other = dyn_cast<Instruction>(user); other && !region.members.contains(other)) {
            region.outputs.push_back(inst); break;
          }
        std::vector<const Value *> targets;
        if (auto *st = dyn_cast<StoreInst>(inst)) targets.push_back(st->getPointerOperand());
        else if (auto *mem = dyn_cast<MemTransferInst>(inst)) targets.push_back(mem->getDest());
        else if (auto *cb = dyn_cast<CallBase>(inst))
          for (unsigned i=0; i<cb->arg_size(); ++i)
            if (cb->paramHasAttr(i, Attribute::StructRet)) targets.push_back(cb->getArgOperand(i));
        for (auto *target : targets) {
          target = base(target);
          if (outside(target) && written.insert(target).second) region.written.push_back(target);
        }
      }
    }
    return regions;
  }
  // Applies the library summary to an inlined call. False when there is no
  // summary to apply; the instructions are then analysed as ordinary code.
  bool summarize(const Region &region, Context *ctx) {
    Site site;
    site.key=region.anchor; site.identity=region.at; site.name=region.name; site.argNames=region.argNames;
    site.region=true; site.retSize=16;
    std::vector<ValueSet> args(region.argValues.size());
    for (unsigned i=0; i<args.size(); ++i)
      for (auto *value : region.argValues[i]) merge(args[i], eval(value, ctx));
    for (auto *value : region.inputs) merge(site.inputs, eval(value, ctx));
    ValueSet result; Effect effect=Effect::Value;
    if (!library(site, ctx, args, result, effect)) return false;
    // What the inlined code produced: a received message is the contents of
    // the summary's result record, anything else is the result itself.
    ValueSet contents = effect==Effect::Payload ? load(offset(result, AnyOffset), ctx) : result;
    if (std::getenv("TINTED_TRACE")) {
      errs()<<"region "<<region.name<<" in "<<llvm::demangle(ctx->fn->getName().str())<<" body="<<(ctx->bodyAt!=nullptr)<<" tags="<<ctx->tags<<" inputs="<<region.inputs.size()<<" outputs="<<region.outputs.size()<<" written="<<region.written.size()<<" args=";
      for (auto &arg : args) errs()<<arg.size()<<",";
      errs()<<" contents:";
      for (auto ref : contents) errs()<<" "<<ref.object<<":"<<ref.offset<<":"<<ref.tags;
      errs()<<"\n";
    }
    for (auto *output : region.outputs)
      if (merge(ctx->values[output], output->getType()->isAggregateType() ? result : contents)) schedule(ctx);
    if (effect!=Effect::None)
      for (auto *target : region.written) store(offset(eval(target, ctx), AnyOffset), contents);
    return true;
  }
  void evaluate(Context *ctx) {
    ctx->queued = false;
    auto &regions = regionsOf(ctx->fn, ctx->bodySp, ctx->bodyAt);
    std::map<const Region *, bool> summaries;
    // Code inlined from a validate function is validated; the rest of the
    // function it was inlined into is not.
    struct Tags {
      Context *ctx; uint64_t saved;
      ~Tags() { ctx->tags = saved; }
    };
    if (std::getenv("TINTED_TRACE")) errs()<<rounds<<" "<<contexts.size()<<" "<<objects.size()<<" "<<llvm::demangle(ctx->fn->getName().str())<<"\n";
    for (auto &inst : instructions(ctx->fn)) {
      if (ctx->bodyAt && !isa<AllocaInst>(inst) && !regions.body.contains(&inst)) continue;
      Tags restore{ctx, ctx->tags};
      if (!regions.validated.empty())
        if (auto found = regions.validated.find(&inst); found != regions.validated.end()) ctx->tags |= found->second;
      if (!regions.of.empty())
        if (auto found = regions.of.find(&inst); found != regions.of.end()) {
          auto [state, fresh] = summaries.try_emplace(found->second, false);
          if (fresh) state->second = summarize(*found->second, ctx);
          if (state->second) continue;
        }
      ValueSet result;
      if (auto *allocation = dyn_cast<AllocaInst>(&inst)) result = reference(allocation, ctx);
      else if (auto *st = dyn_cast<StoreInst>(&inst)) {
        auto pointers = eval(st->getPointerOperand(), ctx);
        auto values = eval(st->getValueOperand(), ctx);
        if (st->getValueOperand()->getType()->isAggregateType()) copy(pointers, values, ctx);
        else store(pointers, values);
      } else if (auto *ld = dyn_cast<LoadInst>(&inst)) {
        auto pointers = eval(ld->getPointerOperand(), ctx);
        result = ld->getType()->isAggregateType() ? pointers : load(pointers, ctx);
      } else if (auto *gep = dyn_cast<GetElementPtrInst>(&inst)) {
        result = offset(eval(gep->getPointerOperand(), ctx), gepOffset(cast<GEPOperator>(gep)));
      } else if (auto *conversion = dyn_cast<CastInst>(&inst)) result = eval(conversion->getOperand(0), ctx);
      else if (auto *extract = dyn_cast<ExtractValueInst>(&inst)) {
        auto base = offset(eval(extract->getAggregateOperand(), ctx),
                           aggregateOffset(extract->getAggregateOperand()->getType(), extract->getIndices()));
        result = extract->getType()->isAggregateType() ? base : load(base, ctx);
      } else if (auto *insert = dyn_cast<InsertValueInst>(&inst)) {
        result = reference(insert, ctx);
        copy(result, eval(insert->getAggregateOperand(), ctx), ctx);
        auto field = offset(result, aggregateOffset(insert->getType(), insert->getIndices()));
        auto values = eval(insert->getInsertedValueOperand(), ctx);
        if (insert->getInsertedValueOperand()->getType()->isAggregateType()) copy(field, values, ctx);
        else store(field, values);
      } else if (auto *phi = dyn_cast<PHINode>(&inst)) {
        for (auto &value : phi->incoming_values()) merge(result, eval(value, ctx));
      } else if (auto *select = dyn_cast<SelectInst>(&inst)) {
        result = eval(select->getTrueValue(), ctx); merge(result, eval(select->getFalseValue(), ctx));
      } else if (auto *binary=dyn_cast<BinaryOperator>(&inst);
                 binary && isa<ConstantInt>(binary->getOperand(1)) &&
                 (binary->getOpcode()==Instruction::Add || binary->getOpcode()==Instruction::Sub ||
                  binary->getOpcode()==Instruction::And || binary->getOpcode()==Instruction::Or)) {
        auto left=eval(binary->getOperand(0),ctx);
        int64_t delta=cast<ConstantInt>(binary->getOperand(1))->getValue().sextOrTrunc(64).getSExtValue();
        for (auto ref:left) {
          if (ref.object<2) { result.insert(ref); continue; }
          if (binary->getOpcode()==Instruction::Add || binary->getOpcode()==Instruction::Sub)
            merge(result,offset({ref},binary->getOpcode()==Instruction::Sub ? -delta : delta));
          else {
            if (ref.offset!=AnyOffset) ref.offset=binary->getOpcode()==Instruction::And ? ref.offset & delta : ref.offset | delta;
            result.insert(ref);
          }
        }
      } else if (auto *cb = dyn_cast<CallBase>(&inst)) { call(*cb, ctx); continue; }
      else if (auto *ret = dyn_cast<ReturnInst>(&inst)) {
        if (auto *value = ret->getReturnValue())
          if (merge(ctx->result, eval(value, ctx))) for (auto *parent : ctx->callers) schedule(parent);
      } else if (auto *rmw = dyn_cast<AtomicRMWInst>(&inst)) {
        result = load(eval(rmw->getPointerOperand(), ctx), ctx);
        store(eval(rmw->getPointerOperand(), ctx), eval(rmw->getValOperand(), ctx));
      } else if (auto *cmp = dyn_cast<AtomicCmpXchgInst>(&inst)) {
        result = reference(cmp, ctx);
        auto ptr = eval(cmp->getPointerOperand(), ctx);
        store(result, load(ptr, ctx)); store(ptr, eval(cmp->getNewValOperand(), ctx));
      } else if (!inst.getType()->isVoidTy()) {
        for (auto &operand : inst.operands())
          if (!isa<BasicBlock>(operand)) merge(result, eval(operand, ctx));
        if (result.empty()) result = reference(&inst, ctx);
      }
      if (merge(ctx->values[&inst], result)) schedule(ctx);
    }
  }
public:
  Analysis(Module &module) : module(module), layout(module.getDataLayout()) {}
  bool run() {
    // Empty annotation calls are meaningful to this analysis, even though
    // they have no program effect. Preserve them and their enclosing function
    // boundaries while simplifying compiler-generated temporaries.
    std::set<std::string> taskTypes;
    for (auto &fn : module) {
      auto name=llvm::demangle(fn.getName().str());
      if (taskBoundary(name)) {
        taskTypes.insert(firstType(name));
        if (name.starts_with("std::thread::spawnhook::add_spawn_hook::<")) {
          auto split=name.find(", ",name.find("::<")+3);
          if (split!=std::string::npos && name.ends_with(">")) taskTypes.insert(name.substr(split+2,name.size()-split-3));
        }
      }
    }
    for (auto &fn : module) {
      auto name=llvm::demangle(fn.getName().str());
      bool boundary=taskBoundary(name) || channelBoundary(name) || runtimeBoundary(name) || name.starts_with("std::rt::lang_start::<");
      for (auto &type:taskTypes)
        boundary |= name==type || name=="core::ptr::drop_glue::<"+type+">" ||
                    (name.starts_with("<"+type+" as ") && name.ends_with("::poll"));
      bool marker = fn.getName().contains("__validate__") ||
                    fn.getName().contains("__sink__") || fn.getName().contains("__source__");
      bool annotated = false;
      for (auto &inst : instructions(fn))
        if (auto *call = dyn_cast<CallBase>(&inst))
          if (auto *callee = call->getCalledFunction())
            annotated |= callee->getName().contains("__validate__") ||
                         callee->getName().contains("__sink__") || callee->getName().contains("__source__");
      if (marker && !fn.isDeclaration()) {
        IRBuilder<> builder(&*fn.getEntryBlock().getFirstInsertionPt());
        builder.CreateCall(Intrinsic::getOrInsertDeclaration(&module, Intrinsic::sideeffect));
      }
      if (marker || annotated || boundary) {
        fn.removeFnAttr(Attribute::AlwaysInline);
        fn.addFnAttr(Attribute::NoInline);
        // The analysis observes parameter provenance and callback identities.
        // Keep these ABIs even when whole-module constant propagation could
        // otherwise erase an argument or a pure Future::poll definition.
        if (!fn.isDeclaration()) fn.setLinkage(GlobalValue::ExternalLinkage);
      }
    }
    // The module is expected to be built at opt-level=1 already; no
    // optimisation pipeline is run here.
    if (auto *path=std::getenv("TINTED_DUMP_IR")) {
      std::error_code error;
      raw_fd_ostream output(path,error);
      if (!error) module.print(output,nullptr);
    }
    for (auto &fn:module) named[llvm::demangle(fn.getName().str())]=&fn;
    for (auto &fn : module) for (auto &inst : instructions(fn)) {
      auto *call = dyn_cast<CallBase>(&inst);
      auto *callee = call ? call->getCalledFunction() : nullptr;
      if (!callee) continue;
      auto name = llvm::demangle(callee->getName().str());
      for (auto kind : {"validate", "sink", "source"}) {
        auto prefix = std::string("__") + kind + "__";
        auto at = name.rfind(prefix);
        if (at == std::string::npos) continue;
        auto id = name.substr(at + prefix.size());
        id = id.substr(0, id.find_first_not_of("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_"));
        if (!labels.contains(id)) labels[id] = labels.size();
        if (labels.size()>63) { errs()<<"FAIL: too many marker identifiers\n"; return false; }
        auto bit = uint64_t(1) << labels[id];
        auto *inlinedAt = inst.getDebugLoc() ? inst.getDebugLoc()->getInlinedAt() : nullptr;
        if (std::string(kind)=="validate") {
          if (inlinedAt) inlinedValidators[inlinedAt] |= bit; else validators[&fn] |= bit;
        }
        if (std::string(kind)=="sink") sinks[&fn] |= bit;
      }
    }
    if (sinks.empty()) { errs()<<"FAIL: no sink annotations\n"; return false; }
    auto *entry = module.getFunction("main");
    if (!entry || entry->isDeclaration()) { errs()<<"FAIL: no main entry point\n"; return false; }
    invoke(entry, nullptr, nullptr, {}, 0);
    while (!work.empty() && !exhausted && rounds++ < 1000000) {
      auto *ctx = work.front(); work.pop_front(); evaluate(ctx);
      if (rounds % 10000 == 0) errs()<<"progress contexts="<<contexts.size()<<" objects="<<objects.size()<<" rounds="<<rounds<<"\n";
    }
    bool ok = !exhausted && work.empty();
    size_t checked = 0;
    for (auto &[_, context] : contexts) {
      auto *ctx = context.get(); auto required = sinks[ctx->fn];
      if (!required || ctx->bodyAt) continue;
      ++checked;
      bool valid = (ctx->tags & required)==required;
      if (!valid && !ctx->fn->arg_empty()) {
        valid = true;
        for (auto &arg : ctx->fn->args()) {
          const auto &values = ctx->values[&arg];
          if (values.empty()) { valid = false; break; }
          for (auto value : values) if ((value.tags & required)!=required) valid = false;
        }
      }
      if (!valid) { errs()<<"FAIL: unchecked sink "<<llvm::demangle(ctx->fn->getName().str())<<"\n"; ok=false; }
      if (std::getenv("TINTED_TRACE")) {
        errs()<<"sink tags="<<ctx->tags<<"\n";
        for (auto &arg:ctx->fn->args()) for (auto ref:ctx->values[&arg])
          errs()<<"  arg="<<arg.getName()<<" object="<<ref.object<<" off="<<ref.offset<<" tags="<<ref.tags<<"\n";
      }
    }
    if (!checked) { errs()<<"FAIL: no reachable sinks (incomplete analysis)\n"; ok=false; }
    if (!work.empty() || exhausted) errs()<<"FAIL: analysis limit reached\n";
    if (std::getenv("TINTED_TRACE")) for (auto &[ctx,call]:unresolved) {
      errs()<<"unresolved in "<<llvm::demangle(ctx->fn->getName().str())<<": "; call->print(errs()); errs()<<"\n";
      auto value=call->getCalledOperand();
      for (auto &arg:ctx->fn->args()) {
        errs()<<"  parameter "<<arg.getName()<<":";
        for (auto ref:ctx->values[&arg]) {
          errs()<<" "<<ref.object<<":"<<ref.offset<<"{";
          for (auto &[off,values]:objects[ref.object].fields) {
            errs()<<off<<"=";
            for (auto val:values) errs()<<val.object<<":"<<val.offset<<",";
            errs()<<";";
          }
          errs()<<"}";
        }
        errs()<<"\n";
      }
      for (unsigned i=0;i<3;++i) {
        errs()<<"  values:";
        for (auto ref:eval(value,ctx)) {
          errs()<<" "<<ref.object<<":"<<ref.offset;
          if (objects[ref.object].value) errs()<<"["<<objects[ref.object].value->getName()<<"]";
          if (opaqueObjects.contains(ref.object)) errs()<<"(opaque)";
        }
        errs()<<"\n";
        if (auto *ld=dyn_cast<LoadInst>(value)) value=ld->getPointerOperand();
        else if (auto *gep=dyn_cast<GetElementPtrInst>(value)) value=gep->getPointerOperand();
        else break;
      }
    }
    errs()<<"contexts="<<contexts.size()<<" objects="<<objects.size()<<" rounds="<<rounds
          <<" unresolved="<<unresolved.size()<<" sink-contexts="<<checked<<"\n";
    outs()<<(ok ? "OK" : "FAIL")<<"\n";
    return ok;
  }
};
}
int main(int argc, char **argv) {
  if (argc!=2) { errs()<<"usage: tinted-ir PROGRAM.bc\n"; return 2; }
  LLVMContext context; SMDiagnostic error;
  auto module = parseIRFile(argv[1], error, context);
  if (!module) { error.print(argv[0], errs()); return 2; }
  Analysis analysis(*module);
  return analysis.run() ? 0 : 1;
}
