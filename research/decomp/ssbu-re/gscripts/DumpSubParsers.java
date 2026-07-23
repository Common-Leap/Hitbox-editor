import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.address.*;
import java.io.*;
public class DumpSubParsers extends GhidraScript {
  DecompInterface dec; PrintWriter out; FunctionManager fm;
  Function ensure(long a) throws Exception { Address ad=toAddr(a); Function f=fm.getFunctionContaining(ad); if(f!=null)return f; disassemble(ad); return createFunction(ad,null); }
  void dump(long a,String l) throws Exception { Function f=ensure(a); if(f==null){out.println("no fn @"+Long.toHexString(a));return;} out.println("\n===== "+l+" : "+f.getName()+" @ "+f.getEntryPoint()+" ====="); DecompileResults r=dec.decompileFunction(f,300,monitor); out.println(r!=null&&r.decompileCompleted()?r.getDecompiledFunction().getC():"FAIL"); }
  public void run() throws Exception {
    fm=currentProgram.getFunctionManager(); dec=new DecompInterface(); dec.openProgram(currentProgram);
    out=new PrintWriter(new FileWriter("subparsers_decomp.txt"));
    dump(0x9a100L,"grtf_textures_9a100");
    dump(0x9a2a0L,"prma_9a2a0");
    dump(0x9a540L,"g3pr_models_9a540");
    dump(0x9a6e0L,"grsn_shaders_9a6e0");
    dump(0x9a840L,"esta_emittersets_9a840");
    dump(0xaf920L,"af920_postwalk");
    dump(0x9aa20L,"emitter_init_9aa20");
    out.close(); println("done");
  }
}
