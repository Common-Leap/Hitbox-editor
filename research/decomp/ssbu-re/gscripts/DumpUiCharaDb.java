import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.mem.*;
import ghidra.program.model.symbol.*;
import java.io.*;
import java.util.*;

/**
 * R-80 — Locate ui_chara_db loader and resident buffer.
 *
 * Searches for ui_chara_db.prc / db_root / disp_order strings, XREFs them,
 * decompiles the containing functions, and reports the pointer chain that
 * holds the resident buffer.  Run with SSBU main_decompressed.bin loaded as
 * ssbu_main in Ghidra; output is ui_chara_db_decomp.txt in the working dir.
 *
 * Intended to be run via: ghidraProj/ssbu_main rep + headless.
 * See docs/roster/PLAN.md Phase 7 and TODO.md R-80.
 */
public class DumpUiCharaDb extends GhidraScript {
    DecompInterface dec;
    PrintWriter out;
    Set<Long> dumped = new HashSet<>();

    String decomp(Function f) {
        DecompileResults res = dec.decompileFunction(f, 120, monitor);
        if (res != null && res.decompileCompleted()) return res.getDecompiledFunction().getC();
        return "DECOMPILE FAILED for " + f.getName() + " @ " + f.getEntryPoint();
    }

    void dumpFn(long ea, String label) throws Exception {
        FunctionManager fm = currentProgram.getFunctionManager();
        Function f = fm.getFunctionContaining(toAddr(ea));
        if (f == null) {
            out.println("\n--- no function containing 0x" + Long.toHexString(ea) + " (" + label + ") ---");
            return;
        }
        long ep = f.getEntryPoint().getOffset();
        if (!dumped.add(ep)) return;
        out.println("\n===== " + label + " : " + f.getName() + " @ " + f.getEntryPoint() + " (" + f.getSignature() + ") =====");
        out.println(decomp(f));
        // Callees one level
        Set<Long> callees = new TreeSet<>();
        InstructionIterator it = currentProgram.getListing().getInstructions(f.getBody(), true);
        while (it.hasNext()) {
            Instruction ins = it.next();
            for (Reference r : ins.getReferencesFrom()) if (r.getReferenceType().isCall()) callees.add(r.getToAddress().getOffset());
        }
        if (!callees.isEmpty()) {
            out.println("\n  -- callees of " + f.getName() + " --");
            for (long c : callees) out.println("  0x" + Long.toHexString(c));
        }
    }

    void findStringRefs(String needle) throws Exception {
        out.println("\n== STRING REFS for \"" + needle + "\" ==");
        Memory mem = currentProgram.getMemory();
        byte[] pat = needle.getBytes("US-ASCII");
        Address at = mem.getMinAddress();
        int hits = 0;
        while (at != null && hits < 80) {
            Address found = mem.findBytes(at, pat, null, true, monitor);
            if (found == null) break;
            hits++;
            MemoryBlock b = mem.getBlock(found);
            out.println("  " + found + " in " + (b == null ? "?" : b.getName()));
            // XREFs TO this address (where the string is used)
            ReferenceIterator rit = currentProgram.getReferenceManager().getReferencesTo(found);
            int xr = 0;
            while (rit.hasNext() && xr < 8) {
                Reference ref = rit.next();
                Address from = ref.getFromAddress();
                Function f = currentProgram.getFunctionManager().getFunctionContaining(from);
                out.println("    -> ref from " + from + (f == null ? "" : " in " + f.getName() + "@" + f.getEntryPoint()));
                xr++;
            }
            at = found.add(1);
        }
        if (hits == 0) out.println("  (no hits)");
    }

    public void run() throws Exception {
        dec = new DecompInterface();
        dec.openProgram(currentProgram);
        out = new PrintWriter(new FileWriter("ui_chara_db_decomp.txt"));
        out.println("program: " + currentProgram.getName());
        out.println("imageBase: " + currentProgram.getImageBase());

        // 1. String inventory
        String[] needles = {
            "ui/param/database/ui_chara_db.prc",
            "ui_chara_db",
            "db_root",
            "disp_order",
            "can_select",
            "fighter_kind",
            "name_id",
            "ui_chara_id",
            "color_num"
        };
        for (String s : needles) findStringRefs(s);

        // 2. Direct hits for the full game path — decompile containers
        Memory mem = currentProgram.getMemory();
        byte[] full = "ui/param/database/ui_chara_db.prc".getBytes("US-ASCII");
        Address at = mem.getMinAddress();
        Set<Long> containers = new HashSet<>();
        while (at != null) {
            Address found = mem.findBytes(at, full, null, true, monitor);
            if (found == null) break;
            ReferenceIterator rit = currentProgram.getReferenceManager().getReferencesTo(found);
            while (rit.hasNext()) {
                Reference ref = rit.next();
                Function f = currentProgram.getFunctionManager().getFunctionContaining(ref.getFromAddress());
                if (f != null) containers.add(f.getEntryPoint().getOffset());
            }
            at = found.add(1);
        }
        out.println("\n== FUNCTIONS CONTAINING REFS TO ui_chara_db path: " + containers.size() + " ==");
        for (long ep : containers) dumpFn(ep, "ui_chara_db_path_user");

        // 3. Also dump known filesystem anchors for context
        // FilesystemInfo pointer is at 0x5331f20 in 13.0.4 text — look near there
        try { dumpFn(0x353eff0L, "queue_directory_release_ctx_anchor"); } catch (Exception e) {}
        try { dumpFn(0x35407a0L, "dir_loader_ctx_anchor"); } catch (Exception e) {}

        // 4. Summary checklist for R-80 deliverable
        out.println("\n== R-80 CHECKLIST ==");
        out.println("  - Loader address: look for function that opens ui_chara_db.prc via prc::open or arc lookup, then parses db_root.");
        out.println("  - Resident buffer: heap alloc that holds parsed db_root entries; pointer stored near FilesystemInfo or UI singleton.");
        out.println("  - Lifecycle: alloc on menu load (CSS enter), freed on menu exit; check xref to free / UI state machine.");
        out.println("  - Pointer chain: document stable anchor (e.g. text+0x5331f20 -> FilesystemInfo -> path_info -> UI db ptr).");
        out.println("  - Next: copy addresses into docs/roster/PLAN.md and implement R-81 offset mapping.");

        out.close();
        println("DumpUiCharaDb done -> ui_chara_db_decomp.txt");
    }
}
