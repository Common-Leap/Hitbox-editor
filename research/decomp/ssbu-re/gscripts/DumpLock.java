import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.address.*;
import java.io.*;
public class DumpLock extends GhidraScript {
  DecompInterface dec; PrintWriter out; FunctionManager fm;
  Function ensure(long a) throws Exception { Address ad=toAddr(a); Function f=fm.getFunctionContaining(ad); if(f!=null)return f; disassemble(ad); return createFunction(ad,null); }
  void dump(long a,String l) throws Exception { Function f=ensure(a); if(f==null){out.println("no fn @"+Long.toHexString(a));return;} out.println("\n===== "+l+" @ "+f.getEntryPoint()+" ====="); DecompileResults r=dec.decompileFunction(f,120,monitor); out.println(r!=null&&r.decompileCompleted()?r.getDecompiledFunction().getC():"FAIL"); }
  public void run() throws Exception {
    fm=currentProgram.getFunctionManager(); dec=new DecompInterface(); dec.openProgram(currentProgram);
    out=new PrintWriter(new FileWriter("lock_decomp.txt"));
    dump(0x39c1410L,"lock_39c1410");
    dump(0x39c1420L,"unlock_39c1420");
    dump(0x39c1490L,"lock_39c1490");
    out.close(); println("done");
  }
}
