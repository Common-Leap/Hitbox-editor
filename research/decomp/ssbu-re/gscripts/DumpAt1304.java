import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import java.io.*;

/**
 * Disassemble a window around the 13.0.4 registration site, then follow the registered function
 * pointer and decompile the glue there, so the partial-frame boolean default can be read in the
 * version-matched image rather than inferred from another build.
 */
public class DumpAt1304 extends GhidraScript {
    public void run() throws Exception {
        long site = 0x2068c90L;
        String env = System.getenv("SITE");
        if (env != null) site = Long.decode(env);
        PrintWriter out = new PrintWriter(new FileWriter("at1304.txt"));

        Address start = toAddr(site - 0x20);
        Address end = toAddr(site + 0x40);
        disassemble(start);
        out.println("== window around " + toAddr(site) + " ==");
        Listing l = currentProgram.getListing();
        InstructionIterator ii = l.getInstructions(start, true);
        while (ii.hasNext()) {
            Instruction in = ii.next();
            if (in.getAddress().compareTo(end) > 0) break;
            out.println("  " + in.getAddress() + "  " + in);
        }

        String target = System.getenv("GLUE");
        if (target != null) {
            long g = Long.decode(target);
            Address ga = toAddr(g);
            disassemble(ga);
            Function f = getFunctionAt(ga);
            if (f == null) f = createFunction(ga, "glue_" + Long.toHexString(g));
            out.println("\n== glue @ " + ga + " ==");
            if (f != null) {
                DecompInterface dec = new DecompInterface();
                dec.setOptions(new DecompileOptions());
                dec.openProgram(currentProgram);
                DecompileResults res = dec.decompileFunction(f, 240, monitor);
                if (res != null && res.decompileCompleted()) out.println(res.getDecompiledFunction().getC());
                else out.println("  <decompile failed>");
                dec.dispose();
            }
            out.println("---- disassembly ----");
            InstructionIterator gi = l.getInstructions(ga, true);
            int n = 0;
            while (gi.hasNext() && n++ < 220) {
                Instruction in = gi.next();
                out.println("  " + in.getAddress() + "  " + in);
                if (in.getMnemonicString().equals("ret")) break;
            }
        }
        out.close();
        println("DumpAt1304: done");
    }
}
