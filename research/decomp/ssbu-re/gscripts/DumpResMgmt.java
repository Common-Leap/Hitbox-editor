import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.symbol.*;
import ghidra.program.model.address.*;
import java.io.*;
import java.util.*;
public class DumpResMgmt extends GhidraScript {
  DecompInterface dec; PrintWriter out; FunctionManager fm; Set<Long> dumped=new HashSet<>();
  Function ensure(long a) throws Exception { Address ad=toAddr(a); Function f=fm.getFunctionContaining(ad); if(f!=null)return f; disassemble(ad); return createFunction(ad,null); }
  void dump(long a,String l) throws Exception { Function f=ensure(a); if(f==null){out.println("no fn @"+Long.toHexString(a));return;} if(!dumped.add(f.getEntryPoint().getOffset()))return; out.println("\n===== "+l+" : "+f.getName()+" @ "+f.getEntryPoint()+" ====="); DecompileResults r=dec.decompileFunction(f,150,monitor); out.println(r!=null&&r.decompileCompleted()?r.getDecompiledFunction().getC():"FAIL"); }
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
    out=new PrintWriter(new FileWriter("resmgmt_decomp.txt"));
    // The "already-loaded" branch helper in FUN_03540860 dir-load, and the recursive refcount.
    dump(0x353eff0L,"FUN_0353eff0_statecheck");
    dump(0x3542d20L,"FUN_03542d20_recursive_refinc");
    // Res file refcount inc/dec — dec triggers eviction when it hits 0.
    dump(0x3540450L,"add_to_res_service_refinc");
    // Who calls res_dec 0x3540560 (release paths — a dir release counterpart lives here).
    callers(0x3540560L,"res_dec");
    // Who calls the dir get-or-load 0x35407a0 (release counterpart may sit beside it).
    callers(0x35407a0L,"dir_get_or_load");
    // The pending-load counter is at res_service+0x4c; find the barrier/wait that reads it,
    // and the loading thread. Dump neighbours of the dir loader.
    dump(0x353e330L,"get_search_path_index");
    dump(0x353e4e0L,"get_file_path_from_search");
    out.close(); println("done");
  }
}
