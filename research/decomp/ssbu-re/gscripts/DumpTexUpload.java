import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.address.*;
public class DumpTexUpload extends GhidraScript {
  DecompInterface dec; java.io.PrintWriter out; FunctionManager fm;
  void dump(long a,String l) throws Exception { Address ad=toAddr(a); Function f=fm.getFunctionContaining(ad); if(f==null){disassemble(ad);f=createFunction(ad,null);} if(f==null){out.println("no fn @"+Long.toHexString(a));return;} out.println("\n===== "+l+" : "+f.getName()+" @ "+f.getEntryPoint()+" ====="); DecompileResults r=dec.decompileFunction(f,180,monitor); out.println(r!=null&&r.decompileCompleted()?r.getDecompiledFunction().getC():"FAIL"); }
  public void run() throws Exception {
    fm=currentProgram.getFunctionManager(); dec=new DecompInterface(); dec.openProgram(currentProgram);
    out=new java.io.PrintWriter(new java.io.FileWriter("texupload_decomp.txt"));
    dump(0x9a100L,"grtf_texture_handler_9a100");
    dump(0x12c1c0L,"cb_12c1c0");
    dump(0x9a230L,"tex_9a230");
    dump(0x93f10L,"chk_93f10");
    dump(0x9a620L,"tex_9a620");
    out.close(); println("done");
  }
}
