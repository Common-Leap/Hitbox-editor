import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.address.*;
import ghidra.program.model.symbol.*;
import java.io.*;
import java.util.*;
public class DumpCtor extends GhidraScript {
  DecompInterface dec; PrintWriter out; FunctionManager fm; Set<Long> done=new HashSet<>();
  Function ensure(long a) throws Exception { Address ad=toAddr(a); Function f=fm.getFunctionContaining(ad); if(f!=null)return f; disassemble(ad); return createFunction(ad,null); }
  void dump(long a,String l) throws Exception { Function f=ensure(a); if(f==null){out.println("no fn @"+Long.toHexString(a));return;} if(!done.add(f.getEntryPoint().getOffset()))return; out.println("\n===== "+l+" : "+f.getName()+" @ "+f.getEntryPoint()+" ====="); DecompileResults r=dec.decompileFunction(f,220,monitor); out.println(r!=null&&r.decompileCompleted()?r.getDecompiledFunction().getC():"FAIL"); }
  public void run() throws Exception {
    fm=currentProgram.getFunctionManager(); dec=new DecompInterface(); dec.openProgram(currentProgram);
    out=new PrintWriter(new FileWriter("ctor_decomp.txt"));
    dump(0x99560L,"effect_set_ctor_99560");
    dump(0x99da0L,"effect_set_dtor_99da0");
    out.close(); println("done");
  }
}
