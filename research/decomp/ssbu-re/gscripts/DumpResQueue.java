import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.symbol.*;
import ghidra.program.model.address.*;
import java.io.*;
import java.util.*;
public class DumpResQueue extends GhidraScript {
  DecompInterface dec; PrintWriter out; FunctionManager fm; Set<Long> dumped=new HashSet<>();
  Function ensure(long a) throws Exception { Address ad=toAddr(a); Function f=fm.getFunctionContaining(ad); if(f!=null)return f; disassemble(ad); return createFunction(ad,null); }
  void dump(long a,String l) throws Exception { Function f=ensure(a); if(f==null){out.println("no fn @"+Long.toHexString(a));return;} if(!dumped.add(f.getEntryPoint().getOffset()))return; out.println("\n===== "+l+" : "+f.getName()+" @ "+f.getEntryPoint()+" ====="); DecompileResults r=dec.decompileFunction(f,150,monitor); out.println(r!=null&&r.decompileCompleted()?r.getDecompiledFunction().getC():"FAIL"); }
  public void run() throws Exception {
    fm=currentProgram.getFunctionManager(); dec=new DecompInterface(); dec.openProgram(currentProgram);
    out=new PrintWriter(new FileWriter("resqueue_decomp.txt"));
    // The enqueue called by FUN_03540860 for each file, and its res-service neighbours.
    dump(0x3542b10L,"enqueue_3542b10");
    dump(0x3540560L,"res_dec_3540560");
    // Callers of the enqueue — likely the loading-thread loop / a wait fn.
    ReferenceManager ref=currentProgram.getReferenceManager();
    ensure(0x3542b10L);
    Set<Long> callers=new TreeSet<>();
    ReferenceIterator it=ref.getReferencesTo(toAddr(0x3542b10L));
    while(it.hasNext()){ Function cf=fm.getFunctionContaining(it.next().getFromAddress()); if(cf!=null)callers.add(cf.getEntryPoint().getOffset()); }
    out.println("\nenqueue callers: "+callers.size());
    for(long c:callers) out.println("  caller @ "+Long.toHexString(c));
    out.close(); println("done");
  }
}
