import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.listing.*;
import java.io.*;
public class Dump860 extends GhidraScript {
  public void run() throws Exception {
    DecompInterface dec = new DecompInterface(); dec.openProgram(currentProgram);
    PrintWriter out = new PrintWriter(new FileWriter("loader860.txt"));
    long[] addrs = {0x3540860L, 0x3540560L};
    for (long a : addrs) {
      var addr = toAddr(a);
      Function fn = getFunctionContaining(addr);
      if (fn == null) { disassemble(addr); fn = createFunction(addr, null); }
      out.println("===== @ " + Long.toHexString(a) + " =====");
      DecompileResults r = dec.decompileFunction(fn, 200, monitor);
      out.println(r!=null&&r.decompileCompleted()? r.getDecompiledFunction().getC():"FAIL");
    }
    out.close(); println("done");
  }
}
