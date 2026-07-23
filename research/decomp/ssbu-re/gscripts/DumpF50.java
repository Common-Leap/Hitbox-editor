import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.address.*;
import java.io.*;
public class DumpF50 extends GhidraScript {
  DecompInterface dec; PrintWriter out; FunctionManager fm;
  void dump(long a,String l) throws Exception { Address ad=toAddr(a); Function f=fm.getFunctionContaining(ad); if(f==null){disassemble(ad);f=createFunction(ad,null);} if(f==null){out.println("no fn @"+Long.toHexString(a));return;} out.println("\n===== "+l+" : "+f.getName()+" @ "+f.getEntryPoint()+" ====="); DecompileResults r=dec.decompileFunction(f,240,monitor); out.println(r!=null&&r.decompileCompleted()?r.getDecompiledFunction().getC():"FAIL"); }
  public void run() throws Exception {
    fm=currentProgram.getFunctionManager(); dec=new DecompInterface(); dec.openProgram(currentProgram);
    out=new PrintWriter(new FileWriter("f50_decomp.txt"));
    dump(0x686320L,"inner_vt_0x50");
    out.close(); println("done");
  }
}
