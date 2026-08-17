import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.mem.*;
import ghidra.program.model.symbol.*;
import java.io.*;
import java.util.*;

/**
 * E1r evidence: from the Lua-visible name string for a MotionModule binding, find the registration
 * site (code refs and 8-byte pointer occurrences), then decompile the surrounding function so the
 * argument-count check and any default the glue supplies can be read directly.
 */
public class DumpLuaReg extends GhidraScript {
    public void run() throws Exception {
        long[] nameAddrs = {0x35befaeL, 0x35befc0L, 0x35bef8dL, 0x35bef5dL};
        String tag = currentProgram.getName().replaceAll("[^A-Za-z0-9_.]", "_");
        PrintWriter out = new PrintWriter(new FileWriter("luareg_" + tag + ".txt"));
        Memory mem = currentProgram.getMemory();

        DecompInterface dec = new DecompInterface();
        dec.setOptions(new DecompileOptions());
        dec.openProgram(currentProgram);

        Set<Function> seen = new LinkedHashSet<>();
        for (long na : nameAddrs) {
            Address a = toAddr(na);
            out.println("\n######## name string @ " + a);

            out.println("  -- direct references --");
            for (Reference r : getReferencesTo(a)) {
                Function f = getFunctionContaining(r.getFromAddress());
                out.println("    " + r.getFromAddress() + " " + r.getReferenceType()
                        + (f == null ? "" : "  in " + f.getName() + " @" + f.getEntryPoint()));
                if (f != null) seen.add(f);
            }

            out.println("  -- 8-byte little-endian pointer occurrences --");
            byte[] pat = new byte[8];
            for (int i = 0; i < 8; i++) pat[i] = (byte) ((na >> (8 * i)) & 0xff);
            Address at = mem.getMinAddress();
            int hits = 0;
            while (at != null && hits < 20) {
                Address found = mem.findBytes(at, pat, null, true, monitor);
                if (found == null) break;
                hits++;
                out.println("    ptr at " + found);
                // A registration entry usually stores {name, fnptr}; show the neighbouring qwords.
                for (int k = -2; k <= 3; k++) {
                    try {
                        Address q = found.add(8L * k);
                        long v = mem.getLong(q);
                        Function tf = getFunctionAt(toAddr(v));
                        out.println(String.format("        [%+d] %s = %016x %s", k, q, v,
                                tf == null ? "" : "-> FUNC " + tf.getName()));
                        if (tf != null) seen.add(tf);
                    } catch (Exception e) {
                        // Neighbour outside the block: nothing to report for this slot.
                    }
                }
                for (Reference r : getReferencesTo(found)) {
                    Function f = getFunctionContaining(r.getFromAddress());
                    out.println("        ref from " + r.getFromAddress()
                            + (f == null ? "" : " in " + f.getName()));
                    if (f != null) seen.add(f);
                }
                at = found.add(1);
            }
        }

        out.println("\n\n======== decompiled candidates (" + seen.size() + ") ========");
        for (Function f : seen) {
            out.println("\n==== " + f.getName() + " @ " + f.getEntryPoint() + " ====");
            DecompileResults res = dec.decompileFunction(f, 180, monitor);
            if (res != null && res.decompileCompleted()) {
                out.println(res.getDecompiledFunction().getC());
            } else {
                out.println("  <decompile failed>");
            }
        }
        dec.dispose();
        out.close();
        println("DumpLuaReg: done, " + seen.size() + " functions");
    }
}
