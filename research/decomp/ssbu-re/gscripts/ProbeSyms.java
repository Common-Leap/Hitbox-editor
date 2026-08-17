import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.mem.*;
import ghidra.program.model.symbol.*;
import java.io.*;
import java.util.*;

/** Inventory a program: blocks, symbol counts, function counts, and raw hits for a byte pattern. */
public class ProbeSyms extends GhidraScript {
    public void run() throws Exception {
        String tag = currentProgram.getName().replaceAll("[^A-Za-z0-9_.]", "_");
        PrintWriter out = new PrintWriter(new FileWriter("probe_syms_" + tag + ".txt"));
        out.println("program: " + currentProgram.getName());
        out.println("language: " + currentProgram.getLanguageID());
        out.println("imageBase: " + currentProgram.getImageBase());

        Memory mem = currentProgram.getMemory();
        out.println("\n== blocks ==");
        for (MemoryBlock b : mem.getBlocks()) {
            out.println("  " + b.getName() + " " + b.getStart() + "-" + b.getEnd()
                    + " size=" + b.getSize() + " x=" + b.isExecute() + " init=" + b.isInitialized());
        }

        SymbolTable st = currentProgram.getSymbolTable();
        int n = 0;
        List<String> sample = new ArrayList<>();
        for (Symbol s : st.getAllSymbols(true)) {
            n++;
            if (sample.size() < 25) sample.add(s.getAddress() + " " + s.getSymbolType() + " " + s.getName());
        }
        out.println("\n== symbols: " + n + " ==");
        for (String s : sample) out.println("  " + s);

        FunctionIterator fi = currentProgram.getFunctionManager().getFunctions(true);
        int fn = 0;
        while (fi.hasNext()) { fi.next(); fn++; }
        out.println("\n== functions: " + fn + " ==");

        String[] needles = {"set_frame_partial", "MotionModule", "set_frame"};
        for (String needle : needles) {
            out.println("\n== raw byte hits for \"" + needle + "\" ==");
            byte[] pat = needle.getBytes("US-ASCII");
            Address at = mem.getMinAddress();
            int hits = 0;
            while (at != null && hits < 40) {
                Address found = mem.findBytes(at, pat, null, true, monitor);
                if (found == null) break;
                hits++;
                MemoryBlock b = mem.getBlock(found);
                out.println("  " + found + " in " + (b == null ? "?" : b.getName()));
                at = found.add(1);
            }
            out.println("  total shown: " + hits);
        }
        out.close();
        println("ProbeSyms: done " + currentProgram.getName());
    }
}
