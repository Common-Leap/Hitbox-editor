import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.symbol.*;
import java.io.*;
import java.util.*;

public class DumpEffect extends GhidraScript {
    public void run() throws Exception {
        FunctionManager fm = currentProgram.getFunctionManager();
        ReferenceManager ref = currentProgram.getReferenceManager();
        DecompInterface dec = new DecompInterface();
        dec.openProgram(currentProgram);
        PrintWriter out = new PrintWriter(new FileWriter("effect_decomp.txt"));
        long[] strs = {0x35832b6L,0x35832d8L,0x35832faL,0x358331cL,0x3583511L};
        HashSet<Long> seen = new HashSet<>();
        out.println("===== STRING-XREF FUNCTIONS =====");
        for (long s : strs) {
            Address sa = toAddr(s);
            ReferenceIterator it = ref.getReferencesTo(sa);
            while (it.hasNext()) {
                Reference r = it.next();
                Function f = fm.getFunctionContaining(r.getFromAddress());
                if (f == null) continue;
                long ep = f.getEntryPoint().getOffset();
                if (!seen.add(ep)) continue;
                out.println("\n--- " + f.getName() + " @ " + f.getEntryPoint() + " ---");
                TreeSet<String> callers = new TreeSet<>();
                ReferenceIterator cit = ref.getReferencesTo(f.getEntryPoint());
                while (cit.hasNext()) {
                    Function cf = fm.getFunctionContaining(cit.next().getFromAddress());
                    if (cf != null) callers.add(cf.getEntryPoint().toString());
                }
                out.println("CALLERS: " + callers);
                out.println(decompile(dec, f));
            }
        }
        out.println("\n===== SEED FUNCTIONS =====");
        for (long s : new long[]{0x1eeb0L,0x20adcL}) {
            Function f = fm.getFunctionContaining(toAddr(s));
            if (f != null) {
                out.println("\n--- seed " + Long.toHexString(s) + " -> " + f.getName() + " @ " + f.getEntryPoint() + " ---");
                out.println(decompile(dec, f));
            }
        }
        out.close();
        println("DumpEffect: wrote effect_decomp.txt");
    }
    String decompile(DecompInterface dec, Function f) {
        DecompileResults r = dec.decompileFunction(f, 90, monitor);
        if (r != null && r.decompileCompleted()) return r.getDecompiledFunction().getC();
        return "<decompile failed>";
    }
}
