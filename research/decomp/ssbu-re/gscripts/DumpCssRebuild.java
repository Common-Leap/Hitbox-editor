import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.mem.*;
import ghidra.program.model.symbol.*;
import java.io.*;
import java.util.*;

/**
 * R-83 — Find CSS grid rebuild trigger.
 *
 * Locates the function that builds the CSS grid from ui_chara_db db_root
 * (sorts by disp_order, handles shared cell 80, skips -1/99).  XREFs disp_order
 * and ui/layout strings, decompiles candidates.
 *
 * Output: css_rebuild_decomp.txt
 */
public class DumpCssRebuild extends GhidraScript {
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
            out.println("\n--- no fn containing 0x" + Long.toHexString(ea) + " (" + label + ") ---");
            return;
        }
        long ep = f.getEntryPoint().getOffset();
        if (!dumped.add(ep)) return;
        out.println("\n===== " + label + " : " + f.getName() + " @ " + f.getEntryPoint() + " =====");
        out.println(decomp(f));
    }

    void findStringRefs(String needle) throws Exception {
        out.println("\n== REFS for \"" + needle + "\" ==");
        Memory mem = currentProgram.getMemory();
        byte[] pat = needle.getBytes("US-ASCII");
        Address at = mem.getMinAddress();
        int hits = 0;
        while (at != null && hits < 60) {
            Address found = mem.findBytes(at, pat, null, true, monitor);
            if (found == null) break;
            hits++;
            out.println("  " + found + " in " + mem.getBlock(found).getName());
            ReferenceIterator rit = currentProgram.getReferenceManager().getReferencesTo(found);
            int xr = 0;
            while (rit.hasNext() && xr < 10) {
                Reference ref = rit.next();
                Address from = ref.getFromAddress();
                Function f = currentProgram.getFunctionManager().getFunctionContaining(from);
                out.println("    -> from " + from + (f == null ? "" : " in " + f.getName() + "@" + f.getEntryPoint()));
                if (f != null) xr++;
                else xr++;
            }
            at = found.add(1);
        }
        if (hits == 0) out.println("  (none)");
    }

    public void run() throws Exception {
        dec = new DecompInterface();
        dec.openProgram(currentProgram);
        out = new PrintWriter(new FileWriter("css_rebuild_decomp.txt"));
        out.println("program: " + currentProgram.getName() + " imageBase: " + currentProgram.getImageBase());

        // 1. Strings that the rebuild must touch
        String[] needles = {
            "disp_order",
            "db_root",
            "ui_chara_db",
            "ui/layout",
            "chara",
            "css",
            "Layout",
            "Pane",
            "SelectScene"
        };
        for (String s : needles) findStringRefs(s);

        // 2. Collect functions that reference disp_order string data
        Memory mem = currentProgram.getMemory();
        byte[] pat = "disp_order".getBytes("US-ASCII");
        Address at = mem.getMinAddress();
        Set<Long> cands = new HashSet<>();
        while (at != null) {
            Address found = mem.findBytes(at, pat, null, true, monitor);
            if (found == null) break;
            ReferenceIterator rit = currentProgram.getReferenceManager().getReferencesTo(found);
            while (rit.hasNext()) {
                Function f = currentProgram.getFunctionManager().getFunctionContaining(rit.next().getFromAddress());
                if (f != null) cands.add(f.getEntryPoint().getOffset());
            }
            at = found.add(1);
        }
        out.println("\n== CANDIDATE REBUILD FUNCTIONS (referenced disp_order): " + cands.size() + " ==");
        for (long ep : cands) dumpFn(ep, "disp_order_user");

        // 3. Also look at stateMachine context (from existing dumps)
        try { dumpFn(0x1a00000L, "stateMachine_anchor_guess"); } catch (Exception e) {}
        // FilesystemInfo-adjacent UI singleton guess
        try { dumpFn(0x5331f20L, "fsinfo_ptr_anchor"); } catch (Exception e) {}

        out.println("\n== R-83 CHECKLIST ==");
        out.println("  - Rebuild fn: iterates db_root, sorts by disp_order, builds pane grid, handles -1/99/shared 80.");
        out.println("  - Call site: invoked on CSS enter; safe to re-invoke after patching resident buffer from UI thread.");
        out.println("  - Args: likely (ui_db_ptr, pane_array, count); document calling convention.");
        out.println("  - Next: hook from roster_pin / live CSS patcher (R-84), prove in Eden with peek before/after.");

        out.close();
        println("DumpCssRebuild done -> css_rebuild_decomp.txt");
    }
}
