import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.symbol.*;
import java.io.*;
import java.util.*;

public class DumpDirLoad extends GhidraScript {
    FunctionManager fm;
    DecompInterface dec;
    PrintWriter out;
    Set<Long> dumped = new HashSet<>();

    Function ensure(long a) throws Exception {
        Address addr = toAddr(a);
        Function f = fm.getFunctionContaining(addr);
        if (f != null) return f;
        disassemble(addr);
        return createFunction(addr, "fn_" + Long.toHexString(a));
    }

    void dump(long a, String label) throws Exception {
        Function f = ensure(a);
        if (f == null) { out.println("\n--- no fn @ " + Long.toHexString(a) + " (" + label + ") ---"); return; }
        if (!dumped.add(f.getEntryPoint().getOffset())) return;
        out.println("\n===== " + label + " : " + f.getName() + " @ " + f.getEntryPoint() + " =====");
        DecompileResults res = dec.decompileFunction(f, 180, monitor);
        out.println(res != null && res.decompileCompleted() ? res.getDecompiledFunction().getC() : "DECOMPILE FAILED");
    }

    public void run() throws Exception {
        fm = currentProgram.getFunctionManager();
        dec = new DecompInterface();
        dec.openProgram(currentProgram);
        out = new PrintWriter(new FileWriter("dirload_decomp.txt"));

        // add_to_res_service + the two path helpers.
        dump(0x3540450L, "add_to_res_service");
        dump(0x353e330L, "get_search_path_index");
        dump(0x353e4e0L, "get_file_path_from_search_path");

        // Callers of load_effects (0x355f8f0) — the fighter/assist effect-dir loaders.
        ReferenceManager ref = currentProgram.getReferenceManager();
        ensure(0x355f8f0L);
        Set<Long> callers = new TreeSet<>();
        ReferenceIterator it = ref.getReferencesTo(toAddr(0x355f8f0L));
        while (it.hasNext()) {
            Function cf = fm.getFunctionContaining(it.next().getFromAddress());
            if (cf != null) callers.add(cf.getEntryPoint().getOffset());
        }
        out.println("\nload_effects callers: " + callers.size());
        int n = 0;
        for (long c : callers) { if (n++ >= 4) break; dump(c, "load_effects_caller"); }

        out.close();
        println("DumpDirLoad done");
    }
}
