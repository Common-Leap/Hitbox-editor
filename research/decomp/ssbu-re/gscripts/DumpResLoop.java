import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.symbol.*;
import ghidra.program.model.address.*;
import java.io.*;
import java.util.*;
public class DumpResLoop extends GhidraScript {
  DecompInterface dec; PrintWriter out; FunctionManager fm; Set<Long> dumped=new HashSet<>();
  Function ensure(long a) throws Exception { Address ad=toAddr(a); Function f=fm.getFunctionContaining(ad); if(f!=null)return f; disassemble(ad); return createFunction(ad,null); }
  void dumpContaining(long a,String l) throws Exception { Function f=ensure(a); if(f==null){out.println("no fn @"+Long.toHexString(a));return;} if(!dumped.add(f.getEntryPoint().getOffset()))return; out.println("\n===== "+l+" : "+f.getName()+" @ "+f.getEntryPoint()+" (probe "+Long.toHexString(a)+") ====="); DecompileResults r=dec.decompileFunction(f,180,monitor); out.println(r!=null&&r.decompileCompleted()?r.getDecompiledFunction().getC():"FAIL"); }
  void callers(long a,String l) throws Exception {
    ensure(a);
    ReferenceManager ref=currentProgram.getReferenceManager();
    Set<Long> cs=new TreeSet<>();
    ReferenceIterator it=ref.getReferencesTo(toAddr(a));
    while(it.hasNext()){ Reference r=it.next(); Function cf=fm.getFunctionContaining(r.getFromAddress()); if(cf!=null)cs.add(cf.getEntryPoint().getOffset()); }
    out.println("\n-- callers of "+l+" ("+Long.toHexString(a)+"): "+cs.size());
    for(long c:cs) out.println("   "+Long.toHexString(c));
  }
  public void run() throws Exception {
    fm=currentProgram.getFunctionManager(); dec=new DecompInterface(); dec.openProgram(currentProgram);
    out=new PrintWriter(new FileWriter("resloop_decomp.txt"));
    dumpContaining(0x35431a4L,"res_load_loop_start");
    dumpContaining(0x3543fd8L,"res_load_loop_refresh");
    dumpContaining(0x3544678L,"inflate");
    // What thread/fn starts the loop — callers of the loop function entry.
    Function lf=fm.getFunctionContaining(toAddr(0x35431a4L));
    if(lf!=null) callers(lf.getEntryPoint().getOffset(),"res_load_loop_fn");
    // The refresh entry may be a separate kick — its callers matter (who wakes the loop).
    Function rf=fm.getFunctionContaining(toAddr(0x3543fd8L));
    if(rf!=null && rf.getEntryPoint().getOffset()!=(lf==null?0:lf.getEntryPoint().getOffset())) callers(rf.getEntryPoint().getOffset(),"res_load_loop_refresh_fn");
    out.close(); println("done");
  }
}
