import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.address.*;
import java.io.*;
import java.util.*;
public class DumpKindReaders extends GhidraScript {
  DecompInterface dec; PrintWriter out; FunctionManager fm;
  public void run() throws Exception {
    fm=currentProgram.getFunctionManager(); dec=new DecompInterface(); dec.openProgram(currentProgram);
    out=new PrintWriter(new FileWriter("kindreaders_decomp.txt"));
    long[] addrs = {0x10733bcL,0x2601158L,0x26037b0L,0x2605f48L,0x26075ccL,0x2607d58L,0x26080a0L,0x260ab9cL,0x43c5e8L,0x2601110L,0x2601278L};
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
