#!/usr/bin/env python3
"""Check eff file handles for samus_atk_bomb."""
import sys
sys.path.insert(0, '/home/leap/Workshop/Hitbox editor')

# Use eff_lib via cargo test
import subprocess, os
os.chdir("/home/leap/Workshop/Hitbox editor")

# Write a Rust test to print the handles
test_code = '''
#[test]
fn test_print_samus_handles() {
    let eff_path = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/samus/ef_samus.eff";
    let eff = match eff_lib::EffFile::from_file(std::path::Path::new(eff_path)) {
        Ok(e) => e,
        Err(_) => { eprintln!("[SKIP] ef_samus.eff not found"); return; }
    };
    eprintln!("[EFF] {} handles", eff.effect_handles.len());
    for (handle, name) in eff.effect_handles.iter().zip(eff.effect_handle_names.iter()) {
        if let Ok(name_str) = name.to_string() {
            if name_str.to_lowercase().contains("bomb") || name_str.to_lowercase().contains("atk") {
                eprintln!("[HANDLE] '{}' -> emitter_set_handle={}", name_str, handle.emitter_set_handle);
            }
        }
    }
    // Also print the first 10 ESET names from the VFXB
    let ptcl_data = eff.resource_data.unwrap_or_default();
    if !ptcl_data.is_empty() {
        if let Ok(ptcl) = crate::effects::PtclFile::parse(&ptcl_data) {
            eprintln!("[PTCL] {} emitter sets", ptcl.emitter_sets.len());
            for (i, set) in ptcl.emitter_sets.iter().enumerate().take(20) {
                eprintln!("[ESET] [{}] '{}'", i, set.name);
            }
        }
    }
}
'''

# Check if test already exists
with open("src/effects.rs", "r") as f:
    content = f.read()

if "test_print_samus_handles" not in content:
    # Add test to effects.rs
    insert_pos = content.rfind("fn test_eff_handle_values()")
    if insert_pos > 0:
        # Insert before test_eff_handle_values
        new_content = content[:insert_pos] + test_code.strip() + "\n\n    " + content[insert_pos:]
        with open("src/effects.rs", "w") as f:
            f.write(new_content)
        print("Added test")
    else:
        print("Could not find insertion point")
else:
    print("Test already exists")

result = subprocess.run(
    ["cargo", "test", "test_print_samus_handles", "--", "--nocapture"],
    capture_output=True, text=True, timeout=120
)
with open("probe_handles_out.txt", "w") as f:
    f.write(result.stdout[-5000:] if len(result.stdout) > 5000 else result.stdout)
    f.write(result.stderr[-5000:] if len(result.stderr) > 5000 else result.stderr)
print("done")
