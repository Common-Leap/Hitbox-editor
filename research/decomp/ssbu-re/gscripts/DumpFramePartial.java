import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.symbol.*;
import ghidra.program.model.data.*;
import java.io.*;
import java.util.*;

/**
 * E1r evidence: locate every `set_frame_partial` surface in the game binary and decompile the
 * Lua-facing glue, so the boolean argument's source-level default is read rather than guessed.
 */
public class DumpFramePartial extends GhidraScript {
    public void run() throws Exception {
        PrintWriter out = new PrintWriter(new FileWriter("frame_partial_decomp.txt"));
        out.println("program: " + currentProgram.getName());

        List<Function> targets = new ArrayList<>();
        out.println("\n== symbols matching frame_partial ==");
        SymbolTable st = currentProgram.getSymbolTable();
        for (Symbol s : st.getAllSymbols(true)) {
            String n = s.getName();
            if (!n.contains("frame_partial")) continue;
            out.println("  " + s.getAddress() + "  " + s.getSymbolType() + "  " + n);
            Function f = getFunctionAt(s.getAddress());
            if (f != null && !targets.contains(f)) targets.add(f);
        }

        out.println("\n== strings containing set_frame_partial ==");
        DataIterator di = currentProgram.getListing().getDefinedData(true);
        while (di.hasNext()) {
            Data d = di.next();
            Object v = d.getValue();
            if (!(v instanceof String)) continue;
            String sv = (String) v;
            if (!sv.contains("set_frame_partial")) continue;
            out.println("  " + d.getAddress() + "  \"" + sv + "\"");
            for (Reference r : getReferencesTo(d.getAddress())) {
                Function f = getFunctionContaining(r.getFromAddress());
                out.println("      ref from " + r.getFromAddress()
                        + (f == null ? "" : "  in " + f.getName() + " @" + f.getEntryPoint()));
                if (f != null && !targets.contains(f)) targets.add(f);
            }
        }

        DecompInterface dec = new DecompInterface();
        DecompileOptions opts = new DecompileOptions();
        dec.setOptions(opts);
        dec.openProgram(currentProgram);

        for (Function f : targets) {
            out.println("\n==== " + f.getName() + " @ " + f.getEntryPoint()
                    + "  params=" + f.getParameterCount() + "  sig=" + f.getSignature());
            out.println("  callers:");
            for (Function c : f.getCallingFunctions(monitor)) {
                out.println("    " + c.getName() + " @" + c.getEntryPoint());
            }
            DecompileResults res = dec.decompileFunction(f, 120, monitor);
            if (res != null && res.decompileCompleted()) {
                out.println(res.getDecompiledFunction().getC());
            } else {
                out.println("  <decompile failed: " + (res == null ? "null" : res.getErrorMessage()) + ">");
            }
        }
        dec.dispose();
        out.close();
        println("DumpFramePartial: done, " + targets.size() + " functions");
    }
}
