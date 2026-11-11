#!/usr/bin/env python3
"""Unchecked controls for tinted-ir: each must be rejected. Run after testcases/run.sh."""
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
ANALYZER = ROOT / 'tinted-ir/target/release/tinted-ir'
MUTATIONS = {
    'pointer': ('same_type_pointer', 'run_unchecked(black_box(harmless));',
                'run_unchecked(black_box(execute));'),
    'method': ('same_signature_method', 'job.peek(2);', 'job.run(2);'),
    'implementation': ('same_trait_impl', 'unchecked(black_box(&Harmless));',
                       'unchecked(black_box(&Dangerous));'),
    'waker': ('waker', 'unchecked(black_box(unsafe { Waker::from_raw(harmless_clone(ptr::null())) }));',
              'unchecked(black_box(unsafe { Waker::from_raw(dangerous_clone(ptr::null())) }));'),
    'callback': ('std_callback', '#[tinted::validate(data)]', ''),
    'channel': ('channel', '#[tinted::validate(data)]', ''),
    'queued_closure': ('queued_closure', '#[tinted::validate(data)]', ''),
    'spawn_hook': ('thread_spawn_hook', '#[tinted::validate(data)]', ''),
}
MIXED_CALLBACK = '''
use std::sync::OnceLock;
static A: OnceLock<u64> = OnceLock::new();
static B: OnceLock<u64> = OnceLock::new();
#[tinted::sink(data)] fn execute() -> u64 { std::hint::black_box(7) }
#[tinted::validate(data)] fn checked() { A.get_or_init(execute); }
fn main() { checked(); B.get_or_init(execute); }
'''
MIXED_TASK = '''
#[tinted::sink(data)] fn execute(x:u64) { std::hint::black_box(x); }
async fn job(x:u64) { execute(x); }
#[tinted::validate(data)] async fn checked() { tokio::spawn(job(7)).await.unwrap(); }
fn main() {
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
        checked().await;
        tokio::spawn(job(8)).await.unwrap();
    });
}
'''
def rejects(path, reason, name):
    result = subprocess.run([str(ANALYZER), str(path)], text=True,
                            capture_output=True, timeout=30)
    if result.returncode != 1 or reason not in result.stdout:
        raise AssertionError(f'{name}: expected {reason!r}, got {result.returncode}\n'
                             f'{result.stdout}{result.stderr}')
    print(f'PASS     unchecked_{name}: rejected', flush=True)


def main():
    sources = {}
    for name, (case, before, after) in MUTATIONS.items():
        source = (ROOT / f'testcases/{case}/src/main.rs').read_text()
        if before not in source:
            raise AssertionError(f'{case}: control replacement no longer matches fixture')
        sources[name] = source.replace(before, after)
    sources['mixed_callback'] = MIXED_CALLBACK
    sources['mixed_task'] = MIXED_TASK
    with tempfile.TemporaryDirectory(prefix='ir-controls-', dir=ROOT / 'target') as temp:
        out = Path(temp)
        for name, source in sources.items():
            rust = out / f'{name}.rs'
            ir = rust.with_suffix('.ll')
            rust.write_text(source)
            command = ['rustc', '--edition=2024', '--crate-name', f'control_{name}',
                       str(rust), '--extern', f'tinted={ROOT}/target/debug/libtinted.so',
                       '--emit=llvm-ir', '-Copt-level=0', '-Cpanic=abort',
                       '-Adead_code', '-o', str(ir)]
            if name == 'mixed_task':
                artifacts = ROOT / 'target/testcases/tokio_task/x86_64-unknown-linux-gnu'
                # rustc only needs metadata to emit IR; the rlibs are stubs without it.
                libraries = list(artifacts.rglob('*.rmeta'))
                tokio = next(p for p in libraries if p.name.startswith('libtokio-'))
                command += ['--extern', f'tokio={tokio}']
                for directory in sorted({p.parent for p in libraries}):
                    command += ['-L', f'dependency={directory}']
            subprocess.run(command, cwd=ROOT, check=True)
            if name == 'mixed_task':
                linked = ir.with_suffix('.bc')
                dependencies = list((ROOT / 'target/testcases/tokio_task/ir').glob('tokio-*.ll'))
                dependencies += list((ROOT / 'target/testcases/tokio_task/ir').glob('pin_project_lite-*.ll'))
                subprocess.run([os.getenv('LLVM_LINK', 'llvm-link-23'), str(ir),
                                *map(str, dependencies), '-o', str(linked)], check=True)
                ir = linked
            rejects(ir, 'unchecked sink', name)


if __name__ == '__main__':
    main()
