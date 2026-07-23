import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.address.*;
public class DumpTexStream extends GhidraScript {
  DecompInterface dec; java.io.PrintWriter out; FunctionManager fm;
  void dump(long a,String l) throws Exception { Address ad=toAddr(a); Function f=fm.getFunctionContaining(ad); if(f==null){disassemble(ad);f=createFunction(ad,null);} if(f==null){out.println("no fn @"+Long.toHexString(a));return;} out.println("\n===== "+l+" : "+f.getName()+" @ "+f.getEntryPoint()+" ====="); DecompileResults r=dec.decompileFunction(f,160,monitor); out.println(r!=null&&r.decompileCompleted()?r.getDecompiledFunction().getC():"FAIL"); }
  public void run() throws Exception {
    fm=currentProgram.getFunctionManager(); dec=new DecompInterface(); dec.openProgram(currentProgram);
    out=new java.io.PrintWriter(new java.io.FileWriter("texstream_decomp.txt"));
    dump(0x93f10L,"guard_93f10");
    dump(0x9a230L,"tex_upload_9a230");
    dump(0x9a2a0L,"prma_model_9a2a0");
    out.close(); println("done");
  }
}
