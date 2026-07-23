import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.pcode.*;
import ghidra.program.model.symbol.*;
import java.io.*;
import java.util.*;

// Decompile the function that CONTAINS the smashline fighter-effect-load hook point
// (0x60bfd8), plus every function it calls, to find how the game makes a fighter's eff
// resident before load_effects — i.e. the real "request file into residency" call
// (add_to_res_service @0x3540450 is refcount-only). Also dump the res-service neighbours.
public class DumpFighterLoad extends GhidraScript {
    FunctionManager fm;
    DecompInterface dec;
    PrintWriter out;
    Set<Long> dumped = new HashSet<>();

    Function ensure(long a) throws Exception {
        Address addr = toAddr(a);
        Function f = fm.getFunctionContaining(addr);
        if (f != null) return f;
        disassemble(addr);
        return createFunction(addr, null);
    }

    String decomp(Function f) {
        DecompileResults res = dec.decompileFunction(f, 200, monitor);
        return (res != null && res.decompileCompleted())
            ? res.getDecompiledFunction().getC() : "DECOMPILE FAILED";
    }

    void dump(long a, String label) throws Exception {
        Function f = ensure(a);
        if (f == null) { out.println("\n--- no fn @ " + Long.toHexString(a) + " (" + label + ") ---"); return; }
        if (!dumped.add(f.getEntryPoint().getOffset())) return;
        out.println("\n===== " + label + " : " + f.getName() + " @ " + f.getEntryPoint() + " =====");
        out.println(decomp(f));
    }

    public void run() throws Exception {
        fm = currentProgram.getFunctionManager();
        dec = new DecompInterface();
        dec.openProgram(currentProgram);
        out = new PrintWriter(new FileWriter("fighterload_decomp.txt"));

        // 1. The function containing the fighter-effect-load call site.
        Function host = ensure(0x60bfd8L);
        if (host == null) { out.println("no host fn @ 0x60bfd8"); out.close(); return; }
        out.println("HOST FN @ " + host.getEntryPoint() + " (contains 0x60bfd8)");
        out.println(decomp(host));
        dumped.add(host.getEntryPoint().getOffset());

        // 2. Every function the host calls (one level) — the residency/request helpers.
        Set<Long> callees = new TreeSet<>();
        InstructionIterator it = currentProgram.getListing().getInstructions(host.getBody(), true);
        while (it.hasNext()) {
            Instruction ins = it.next();
            for (Reference r : ins.getReferencesFrom()) {
                if (r.getReferenceType().isCall()) callees.add(r.getToAddress().getOffset());
            }
        }
        out.println("\n=== host callees: " + callees.size() + " ===");
        for (long c : callees) out.println("  callee @ " + Long.toHexString(c));
        for (long c : callees) dump(c, "host_callee");

        // 3. Res-service neighbours around add_to_res_service (0x3540450): the load-request
        //    and unload siblings usually sit adjacent.
        dump(0x3540560L, "res_neighbor_3540560");   // the "unload/dec" counterpart from load_effects
        dump(0x353e5d0L, "helper_353e5d0");          // called inside load_effects folder walk

        out.close();
        println("DumpFighterLoad done");
    }
}
