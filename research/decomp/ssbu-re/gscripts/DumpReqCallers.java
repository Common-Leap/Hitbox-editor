import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.address.*;
import java.io.*;
import java.util.*;
public class DumpReqCallers extends GhidraScript {
  public void run() throws Exception {
    FunctionManager fm=currentProgram.getFunctionManager();
    DecompInterface dec=new DecompInterface(); dec.openProgram(currentProgram);
    PrintWriter out=new PrintWriter(new FileWriter("reqcallers_decomp.txt"));
    long[] addrs = {0x36dc28L,0x36e2d8L,0x36eefcL,0x375928L,0x375b0cL};
    Set<String> seen = new HashSet<>();
    for (long a : addrs) {
      Address ad = toAddr(a);
      Function f = fm.getFunctionContaining(ad);
      if (f==null) { disassemble(ad); f = createFunction(ad,null); }
      if (f==null) { out.println("no fn containing "+Long.toHexString(a)); continue; }
      if (!seen.add(f.getEntryPoint().toString())) continue;
      out.println("\n===== containing "+Long.toHexString(a)+" : "+f.getName()+" @ "+f.getEntryPoint()+" =====");
      DecompileResults r=dec.decompileFunction(f,300,monitor);
      out.println(r!=null&&r.decompileCompleted()?r.getDecompiledFunction().getC():"FAIL");
    }
    out.close(); println("done");
  }
}
