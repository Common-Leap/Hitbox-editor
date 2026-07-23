import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.symbol.*;
import ghidra.program.model.pcode.*;
import java.io.*;
import java.util.*;

public class DumpAddrs extends GhidraScript {
    DecompInterface dec;
    FunctionManager fm;
    PrintWriter out;
    public void run() throws Exception {
        fm = currentProgram.getFunctionManager();
        dec = new DecompInterface(); dec.openProgram(currentProgram);
        out = new PrintWriter(new FileWriter("decomp_out.txt"));
        String argstr=null; try{ argstr=new java.util.Scanner(new java.io.File("targets.txt")).useDelimiter("\\A").next().trim().replaceAll("\\s+",","); }catch(Exception e){}
        if (argstr == null) argstr = "0x1a730";
        for (String s : argstr.split(",")) {
            long v = Long.decode(s.trim());
            dumpFunc(v);
        }
        out.close();
        println("wrote decomp_out.txt");
    }
    Function ensure(long v) {
        Address a = toAddr(v);
        Function f = fm.getFunctionContaining(a);
        if (f == null) {
            try { disassemble(a); f = createFunction(a, null); } catch (Exception e) {}
            if (f == null) f = fm.getFunctionContaining(a);
        }
        return f;
    }
    void dumpFunc(long v) {
        Function f = ensure(v);
        if (f == null) { out.println("\n### "+Long.toHexString(v)+": <no function>"); return; }
        out.println("\n############ "+f.getName()+" @ "+f.getEntryPoint()+" ############");
        DecompileResults r = dec.decompileFunction(f, 90, monitor);
        if (r != null && r.decompileCompleted()) {
            out.println(r.getDecompiledFunction().getC());
            for (Function c : f.getCallingFunctions(monitor)) callees.add("CALLER "+c.getEntryPoint()+" "+c.getName());
            // list callees
            TreeSet<String> callees = new TreeSet<>();
            for (Function c : f.getCalledFunctions(monitor)) callees.add(c.getEntryPoint()+" "+c.getName());
            out.println("// CALLEES: "+callees);
        } else out.println("<decompile failed>");
    }
}
