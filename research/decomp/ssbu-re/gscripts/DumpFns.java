import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import java.io.*;
import java.util.*;

public class DumpFns extends GhidraScript {
    DecompInterface dec; FunctionManager fm; PrintWriter out;
    public void run() throws Exception {
        fm = currentProgram.getFunctionManager();
        dec = new DecompInterface(); dec.openProgram(currentProgram);
        out = new PrintWriter(new FileWriter("decomp_out.txt"));
        Scanner sc = new Scanner(new File("targets.txt"));
        out.println("// targets read:");
        while (sc.hasNext()) {
            String tok = sc.next().trim();
            if (tok.isEmpty()) continue;
            dumpFunc(Long.decode(tok));
        }
        out.close(); println("DumpFns done");
    }
    Function ensure(long v){ Address a=toAddr(v); Function f=fm.getFunctionContaining(a);
        if(f==null){ try{ disassemble(a); f=createFunction(a,null);}catch(Exception e){} if(f==null) f=fm.getFunctionContaining(a);} return f; }
    void dumpFunc(long v){
        Function f=ensure(v);
        if(f==null){ out.println("\n### "+Long.toHexString(v)+": <no function>"); return; }
        out.println("\n############ "+f.getName()+" @ "+f.getEntryPoint()+" ############");
        DecompileResults r=dec.decompileFunction(f,90,monitor);
        if(r!=null&&r.decompileCompleted()) out.println(r.getDecompiledFunction().getC());
        else out.println("<decompile failed>");
        TreeSet<String> ce=new TreeSet<>(); for(Function c:f.getCalledFunctions(monitor)) ce.add(c.getEntryPoint().toString());
        out.println("// CALLEES: "+ce);
    }
}
