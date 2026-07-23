import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import java.io.*;
import java.util.*;

public class DumpLoadEffects extends GhidraScript {
    public void run() throws Exception {
        FunctionManager fm = currentProgram.getFunctionManager();
        DecompInterface dec = new DecompInterface();
        dec.openProgram(currentProgram);
        PrintWriter out = new PrintWriter(new FileWriter("load_effects_decomp.txt"));
        long[] targets = {0x355f8f0L, 0x3563720L, 0x60bfd8L};
        Set<Long> dumped = new HashSet<>();
        Deque<Long> queue = new ArrayDeque<>();
        for (long t : targets) queue.add(t);
        int depth = 0;
        // Dump targets plus one level of callees of load_effects itself.
        while (!queue.isEmpty() && dumped.size() < 24) {
            long a = queue.poll();
            Function f = fm.getFunctionContaining(toAddr(a));
            if (f == null) { out.println("\n--- no function @ " + Long.toHexString(a) + " ---"); continue; }
            long ep = f.getEntryPoint().getOffset();
            if (!dumped.add(ep)) continue;
            out.println("\n===== " + f.getName() + " @ " + f.getEntryPoint() + " (asked " + Long.toHexString(a) + ") =====");
            DecompileResults res = dec.decompileFunction(f, 120, monitor);
            if (res != null && res.decompileCompleted()) {
                out.println(res.getDecompiledFunction().getC());
            } else {
                out.println("DECOMPILE FAILED");
            }
            // queue callees only for the primary target (load_effects)
            if (ep == 0x355f8f0L) {
                for (Function callee : f.getCalledFunctions(monitor)) {
                    queue.add(callee.getEntryPoint().getOffset());
                }
            }
        }
        out.close();
        println("DumpLoadEffects: done");
    }
}
