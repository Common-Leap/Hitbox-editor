import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.address.*;
import java.io.*;
import java.util.*;
public class DumpFollow extends GhidraScript {
  DecompInterface dec; PrintWriter out; FunctionManager fm;
  void dump(long a,String l) throws Exception { Address ad=toAddr(a); Function f=fm.getFunctionContaining(ad); if(f==null){disassemble(ad);f=createFunction(ad,null);} if(f==null){out.println("no fn @"+Long.toHexString(a));return;} out.println("\n===== "+l+" containing "+Long.toHexString(a)+" : "+f.getName()+" @ "+f.getEntryPoint()+" ====="); DecompileResults r=dec.decompileFunction(f,240,monitor); out.println(r!=null&&r.decompileCompleted()?r.getDecompiledFunction().getC():"FAIL"); }
  public void run() throws Exception {
    fm=currentProgram.getFunctionManager(); dec=new DecompInterface(); dec.openProgram(currentProgram);
    out=new PrintWriter(new FileWriter("follow_decomp.txt"));
    dump(0x2017730L,"req_follow_shim");
    dump(0x44de70L,"req_impl_vt68");
    out.close(); println("done");
  }
}
