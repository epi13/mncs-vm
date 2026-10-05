//! Phase and allocation accounting for a frozen artifact and batch calls.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(l.size() as u64, Relaxed);
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(n as u64, Relaxed);
        System.realloc(p, l, n)
    }
}
#[global_allocator]
static ALLOC: Counting = Counting;
fn phase<T>(name: &str, f: impl FnOnce() -> T) -> T {
    let a = ALLOCS.load(Relaxed);
    let b = BYTES.load(Relaxed);
    let t = Instant::now();
    let out = f();
    eprintln!(
        "{}",
        serde_json::json!({"phase":name,"seconds":t.elapsed().as_secs_f64(),"allocations":ALLOCS.load(Relaxed)-a,"allocated_bytes":BYTES.load(Relaxed)-b})
    );
    out
}
#[derive(serde::Deserialize)]
struct Call {
    id: String,
    request: mncs_model::ExecutionRequest,
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let bytes = phase("read", || std::fs::read(&args[1]).unwrap());
    let artifact = phase("decode", || {
        serde_json::from_slice::<mncs_vm::artifact::VmArtifact>(&bytes).unwrap()
    });
    drop(bytes);
    let admitted = phase("admit", || {
        mncs_vm::admit::admit_artifact(artifact).unwrap()
    });
    let mut session = phase("session", || mncs_vm::session::Session::open(&admitted));
    let calls: Vec<Call> = serde_json::from_slice(&std::fs::read(&args[2]).unwrap()).unwrap();
    let envelope: Option<mncs_vm::resource::ResourceEnvelope> = args
        .get(3)
        .map(|path| serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap());
    let caps = mncs_vm::capability::CapabilityEnv::empty();
    for run in 0..2 {
        phase(&format!("execute-{run}"), || {
            let mut digest = sha2::Sha256::new();
            use sha2::Digest;
            for c in &calls {
                let spec =
                    mncs_vm::session::CallSpec::from_request(c.request.clone(), envelope.as_ref())
                        .unwrap();
                let (outcome, record) = session.call(&caps, spec);
                assert_eq!(outcome, mncs_vm::outcome::Outcome::Completed, "{}", c.id);
                digest.update(record.return_digest.as_bytes());
            }
            eprintln!("return-digest {run} {:x}", digest.finalize());
        });
    }
}
