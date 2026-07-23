import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import java.io.*;
import java.util.*;

public class DumpLoadEffects2 extends GhidraScript {
    FunctionManager fm;
    DecompInterface dec;
    PrintWriter out;
    Set<Long> dumped = new HashSet<>();

    Function ensureFunction(long a) throws Exception {
        Address addr = toAddr(a);
        Function f = fm.getFunctionContaining(addr);
        if (f != null) return f;
        disassemble(addr);
        f = createFunction(addr, "fn_" + Long.toHexString(a));
        return f != null ? f : fm.getFunctionContaining(addr);
    }

    void dump(long a, boolean followCallees, int depth) throws Exception {
        if (depth > 1 || dumped.size() > 20) return;
        Function f = ensureFunction(a);
        if (f == null) { out.println("\n--- STILL no function @ " + Long.toHexString(a) + " ---"); return; }
        long ep = f.getEntryPoint().getOffset();
        if (!dumped.add(ep)) return;
        out.println("\n===== " + f.getName() + " @ " + f.getEntryPoint() + " =====");
        DecompileResults res = dec.decompileFunction(f, 180, monitor);
        if (res != null && res.decompileCompleted()) {
            out.println(res.getDecompiledFunction().getC());
        } else {
            out.println("DECOMPILE FAILED");
        }
        if (followCallees) {
            for (Function callee : f.getCalledFunctions(monitor)) {
                dump(callee.getEntryPoint().getOffset(), false, depth + 1);
            }
        }
    }

    public void run() throws Exception {
        fm = currentProgram.getFunctionManager();
        dec = new DecompInterface();
        dec.openProgram(currentProgram);
        out = new PrintWriter(new FileWriter("load_effects_decomp2.txt"));
        dump(0x355f8f0L, true, 0);   // load_effects + its callees
        dump(0x3563720L, false, 0);  // unload_effects
        out.close();
        println("DumpLoadEffects2: done");
    }
}
