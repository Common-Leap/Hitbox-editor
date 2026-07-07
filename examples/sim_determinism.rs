//! Bisection probe: is the particle *simulation* deterministic across processes?
//! Prints a pure-CPU fingerprint (count + hash) per frame. Run twice and compare — if the
//! hashes match across runs, any render nondeterminism is in the GPU/render path, not the sim.
//!
//!   cargo run --example sim_determinism -- <fighter> <handle>

fn main() {
    let fighter = std::env::args().nth(1).unwrap_or_else(|| "samus".into());
    let handle = std::env::args().nth(2).unwrap_or_else(|| "samus_atk_bomb".into());
    let Some(path) = hitbox_editor::scratch_dirs::resolve_fighter_eff(&fighter) else {
        eprintln!("no effect export for '{fighter}'");
        return;
    };
    for f in [0u32, 8, 24, 48] {
        match hitbox_editor::regression::sim_fingerprint_from_file(&path, &handle, f, "Trans") {
            Some((n, h)) => println!("f{f}: count={n} hash={h:016x}"),
            None => println!("f{f}: load/sim failed"),
        }
    }
}
