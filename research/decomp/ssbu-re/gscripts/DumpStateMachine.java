import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.address.*;
import java.util.*;
public class DumpStateMachine extends GhidraScript {
  DecompInterface dec; java.io.PrintWriter out; FunctionManager fm;
  void dump(long a) throws Exception { Address ad=toAddr(a); Function f=fm.getFunctionContaining(ad); if(f==null){disassemble(ad);f=createFunction(ad,null);} if(f==null){out.println("no fn @"+Long.toHexString(a));return;} out.println("\n===== containing "+Long.toHexString(a)+" : "+f.getName()+" @ "+f.getEntryPoint()+" ====="); DecompileResults r=dec.decompileFunction(f,200,monitor); out.println(r!=null&&r.decompileCompleted()?r.getDecompiledFunction().getC():"FAIL"); }
  public void run() throws Exception {
    fm=currentProgram.getFunctionManager(); dec=new DecompInterface(); dec.openProgram(currentProgram);
    out=new java.io.PrintWriter(new java.io.FileWriter("statemachine_decomp.txt"));
    Set<String> seen=new HashSet<>();
    for (long a : new long[]{0x934f4L,0x93744L,0x93874L,0x936ccL}) {
      Function f=fm.getFunctionContaining(toAddr(a));
      String key = f==null?("?"+a):f.getEntryPoint().toString();
      if(seen.add(key)) dump(a);
    }
    out.close(); println("done");
  }
}
