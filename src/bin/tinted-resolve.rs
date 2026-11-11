//! Resolves indirect calls (function pointers and trait objects) from LLVM IR
//! built with `-Zsanitizer=kcfi`, and prints them as extra call-graph edges
//! that `tinted-check` reads next to the `opt` call graph.
//!
//! With KCFI, rustc gives every indirect call a `"kcfi"(i32 <type id>)`
//! operand bundle and every function a `!kcfi_type` with the type id of its
//! signature. For a method called on a trait object the type id is that of the
//! trait method, so a call site and every implementation agree. A call site
//! can only call a function that has its type id and whose address is taken
//! somewhere (used as a value, or stored in a vtable).
//!
//! The match is on types, not on values, so a call site gets every function of
//! that type. That is exact for most traits and very broad for a few common
//! types: a call to drop glue through a vtable gets every drop glue that is in
//! one, and a call to a boxed `FnOnce()` gets every closure of that type.
//!
//! A spawned thread's closure is called from the prebuilt standard library, so
//! no call site for it is in the IR. Threads are paired by name instead:
//! `std::thread`'s `spawn_unchecked::<F, T>` gets an edge to the entry point of
//! the closure it hands to the new thread, and that entry point gets no
//! type-matched callers, since nothing else can call it. This relies on how
//! this toolchain's standard library names those functions.
//!
//! Not resolved: other calls made by code that is not in the IR, channels,
//! tasks spawned on other runtimes, inline assembly, and targets that are only
//! loaded at run time. Two methods of one trait with the same signature share
//! a type id. A function that `llvm-link` renames (a duplicate local symbol
//! gets a `.N` suffix) only gets edges under its original name.

use {
    clap::Parser,
    std::{
        collections::{BTreeMap, BTreeSet, HashMap, HashSet},
        fs::File,
        io::{BufRead, BufReader, BufWriter, Write},
        sync::atomic::{AtomicUsize, Ordering},
        thread,
    },
};

/// Resolves indirect calls from LLVM IR built with `-Zsanitizer=kcfi`.
#[derive(Parser)]
struct Args {
    /// Textual LLVM IR files (`.ll`) built with `-Zsanitizer=kcfi`
    #[arg(required = true)]
    ir: Vec<String>,
}

/// What one IR file says about indirect calls.
#[derive(Default)]
struct Scan {
    /// Function name -> KCFI type id, for definitions and declarations.
    type_ids: HashMap<String, i32>,
    /// Names used as a value (`ptr @name`): address-taken functions, and
    /// other globals.
    taken: HashSet<String>,
    /// (function containing the call, type id the call expects).
    sites: Vec<(String, i32)>,
}

/// The name in the first `@name` or `@"name"` of `text`, and the text after it.
fn global(text: &str) -> Option<(&str, &str)> {
    let rest = &text[text.find('@')? + 1..];
    if let Some(quoted) = rest.strip_prefix('"') {
        let end = quoted.find('"')?;
        Some((&quoted[..end], &quoted[end + 1..]))
    } else {
        let is_name = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$' | '-');
        let end = rest.find(|c| !is_name(c)).unwrap_or(rest.len());
        Some((&rest[..end], &rest[end..]))
    }
}

/// Whether `text`, the start of a call instruction, calls a named function
/// (`call <type> @name(`) rather than a value.
fn calls_named_function(text: &str) -> bool {
    let mut rest = text;
    while let Some((_, after)) = global(rest) {
        if after.starts_with('(') {
            return true;
        }
        rest = after;
    }
    false
}

/// The number that follows the first `prefix` in `text`.
fn number_after<T: std::str::FromStr>(text: &str, prefix: &str) -> Option<T> {
    let rest = &text[text.find(prefix)? + prefix.len()..];
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '-')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn scan(path: &str) -> Scan {
    let mut scan = Scan::default();
    let file = File::open(path).unwrap_or_else(|err| panic!("cannot open {path}: {err}"));
    let mut reader = BufReader::new(file);
    // A function's `!kcfi_type !N` names a metadata node `!N = !{i32 <type id>}`
    // that comes later in the file. Node numbers are per file.
    let mut nodes_of_functions: Vec<(String, u32)> = vec![];
    let mut type_id_nodes: HashMap<u32, i32> = HashMap::new();
    let mut current: Option<String> = None;
    let mut buf = vec![];
    while reader.read_until(b'\n', &mut buf).unwrap() > 0 {
        let line = String::from_utf8_lossy(&buf);
        if let Some(rest) = line.strip_prefix('!') {
            if let Some((node, value)) = rest.split_once(" = !{i32 ")
                && let Some(value) = value.trim_end().strip_suffix('}')
                && let (Ok(node), Ok(value)) = (node.parse(), value.parse())
            {
                type_id_nodes.insert(node, value);
            }
        } else if line.starts_with("define ") || line.starts_with("declare ") {
            let name = global(&line).map(|(name, _)| name);
            if let (Some(name), Some(node)) = (name, number_after(&line, "!kcfi_type !")) {
                nodes_of_functions.push((name.to_string(), node));
            }
            if line.starts_with("define ") {
                current = name.map(str::to_string);
            }
        } else if line.starts_with('}') {
            current = None;
        } else {
            // `ptr @name` is a use of the address, unless it is the callee of
            // a direct call that returns a pointer (`call ptr @name(...)`).
            let mut rest = &*line;
            while let Some(at) = rest.find("ptr @") {
                let Some((name, after)) = global(&rest[at..]) else { break };
                if !after.starts_with('(') && !scan.taken.contains(name) {
                    scan.taken.insert(name.to_string());
                }
                rest = after;
            }
            // A call that LLVM has since made direct (after inlining) keeps
            // its type id, but is already an edge in the call graph.
            const BUNDLE: &str = "\"kcfi\"(i32 ";
            if let Some(caller) = &current
                && let Some(bundle) = line.find(BUNDLE)
                && !calls_named_function(&line[..bundle])
                && let Some(type_id) = number_after(&line[bundle..], BUNDLE)
            {
                scan.sites.push((caller.clone(), type_id));
            }
        }
        buf.clear();
    }
    for (name, node) in nodes_of_functions {
        if let Some(&type_id) = type_id_nodes.get(&node) {
            scan.type_ids.insert(name, type_id);
        }
    }
    scan
}

/// Pairs each `std::thread` spawn with the entry point of the thread it
/// starts, as (spawner, entry point), and counts entry points left unpaired.
///
/// Both are instances of generic standard library code, so both names carry
/// the spawned closure's type `F` and its result type `T`:
/// - `spawn_unchecked::<F, T>` boxes a closure of its own, `{closure#1}`,
///   and hands it to the new thread;
/// - `<spawn_unchecked<F, T>::{closure#1} as FnOnce<()>>::call_once::{shim:vtable#0}`
///   is what the new thread calls.
fn thread_spawns<'a>(names: impl Iterator<Item = &'a String>) -> (Vec<(&'a str, &'a str)>, usize) {
    const SPAWN: &str = "std::thread::lifecycle::spawn_unchecked::<";
    const ENTRY_START: &str = "<std::thread::lifecycle::spawn_unchecked<";
    const ENTRY_END: &str =
        "::{closure#1} as core::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}";

    // The spawner's name with crate hashes -> mangled names. The names are
    // recognised without hashes and paired with them, so that two versions of
    // one crate stay apart.
    let mut spawners: HashMap<String, Vec<&str>> = HashMap::new();
    let mut entries: HashMap<String, Vec<&str>> = HashMap::new();
    for name in names.filter(|name| name.contains("spawn_unchecked")) {
        let demangled = rustc_demangle::demangle(name);
        let (pretty, hashed) = (format!("{demangled:#}"), demangled.to_string());
        // A closure inside the spawner has a name that goes on past `<F, T>`.
        if pretty.starts_with(SPAWN) && pretty.ends_with('>') {
            spawners.entry(hashed).or_default().push(name);
        } else if pretty.starts_with(ENTRY_START) && pretty.ends_with(ENTRY_END) {
            // `<std[..]::..::spawn_unchecked<F, T>::{closure#1} as ..` names
            // the spawner as a type; as a function it is `spawn_unchecked::<F, T>`.
            let Some(end) = hashed.rfind("::{closure#1} as ") else { continue };
            let spawner = hashed[1..end].replacen("spawn_unchecked<", "spawn_unchecked::<", 1);
            entries.entry(spawner).or_default().push(name);
        }
    }
    let mut pairs = vec![];
    let mut unpaired = 0;
    for (spawner, entry) in &entries {
        match (spawners.get(spawner).map(Vec::as_slice), entry.as_slice()) {
            (Some([spawner]), [entry]) => pairs.push((*spawner, *entry)),
            _ => unpaired += entry.len(),
        }
    }
    pairs.sort_unstable();
    (pairs, unpaired)
}

fn main() {
    let Args { ir } = Args::parse();

    // Scan the files in parallel; each thread takes the next unscanned file.
    let next = AtomicUsize::new(0);
    let threads = thread::available_parallelism().map_or(1, |n| n.get()).min(ir.len());
    let scans: Vec<Scan> = thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut scans = vec![];
                    while let Some(path) = ir.get(next.fetch_add(1, Ordering::Relaxed)) {
                        scans.push(scan(path));
                    }
                    scans
                })
            })
            .collect();
        workers.into_iter().flat_map(|worker| worker.join().unwrap()).collect()
    });
    let mut type_ids: HashMap<String, i32> = HashMap::new();
    let mut taken: HashSet<String> = HashSet::new();
    let mut sites: Vec<(String, i32)> = vec![];
    for scan in scans {
        type_ids.extend(scan.type_ids);
        taken.extend(scan.taken);
        sites.extend(scan.sites);
    }

    let (spawns, unpaired) = thread_spawns(type_ids.keys());
    let thread_entries: HashSet<&str> = spawns.iter().map(|&(_, entry)| entry).collect();

    // type id -> the functions an indirect call of that type can reach.
    let mut candidates: HashMap<i32, BTreeSet<&str>> = HashMap::new();
    for (name, type_id) in &type_ids {
        if taken.contains(name) && !thread_entries.contains(name.as_str()) {
            candidates.entry(*type_id).or_default().insert(name);
        }
    }

    let (mut resolved, mut none) = (0, 0);
    let mut callers: BTreeMap<&str, BTreeSet<i32>> = BTreeMap::new();
    for (caller, type_id) in &sites {
        if candidates.contains_key(type_id) {
            resolved += 1;
            callers.entry(caller).or_default().insert(*type_id);
        } else {
            none += 1;
        }
    }

    // Same syntax as `opt -passes=print-callgraph`, so that tinted-check
    // (and rustfilt) can read it; the call site slot holds the type id.
    let mut out = BufWriter::new(std::io::stdout().lock());
    let mut edges = 0;
    for (caller, type_ids) in &callers {
        writeln!(out, "Call graph node for function: '{caller}'<<kcfi>>  #uses=0").unwrap();
        for type_id in type_ids {
            for target in &candidates[type_id] {
                writeln!(out, "  CS<kcfi {type_id}> calls function '{target}'").unwrap();
                edges += 1;
            }
        }
        writeln!(out).unwrap();
    }
    for (spawner, entry) in &spawns {
        writeln!(out, "Call graph node for function: '{spawner}'<<thread>>  #uses=0").unwrap();
        writeln!(out, "  CS<thread> calls function '{entry}'\n").unwrap();
    }
    out.flush().unwrap();

    eprintln!("{} indirect call site(s) have a KCFI type id", sites.len());
    eprintln!("  {resolved} resolved, to {edges} edge(s)");
    eprintln!("  {none} left unresolved: no candidate target in the IR");
    eprintln!("{} thread spawn(s) paired with their entry point by name", spawns.len());
    if unpaired > 0 {
        eprintln!("  {unpaired} thread entry point(s) left unpaired");
    }
    if spawns.is_empty() {
        eprintln!("  warning: none found; the standard library's names may have changed");
    }
}
