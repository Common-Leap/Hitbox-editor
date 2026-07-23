# Ghidra headless (Jython): decompile string-xref + seed functions -> effect_decomp.txt
from ghidra.app.decompiler import DecompInterface
from ghidra.util.task import ConsoleTaskMonitor

fm = currentProgram.getFunctionManager()
af = currentProgram.getAddressFactory()
ref = currentProgram.getReferenceManager()
dec = DecompInterface(); dec.openProgram(currentProgram)
mon = ConsoleTaskMonitor()
out = open("effect_decomp.txt","w")
def w(s): out.write(s+"\n")

def a(x): return af.getAddress(hex(x).rstrip("L").replace("0x",""))
def func_at(addr): return fm.getFunctionContaining(addr)
def decompile(f):
    if f is None: return "<no function>"
    r = dec.decompileFunction(f, 90, mon)
    if r and r.decompileCompleted(): return r.getDecompiledFunction().getC()
    return "<decompile failed: %s>" % (f.getName())

string_addrs = [0x35832b6,0x35832d8,0x35832fa,0x358331c,0x3583511]
seen=set()
w("===== STRING-XREF FUNCTIONS =====")
for sa in string_addrs:
    for r in ref.getReferencesTo(a(sa)):
        fn=func_at(r.getFromAddress())
        if fn and fn.getEntryPoint().getOffset() not in seen:
            seen.add(fn.getEntryPoint().getOffset())
            w("\n--- %s @ %s ---" % (fn.getName(), fn.getEntryPoint()))
            # also list callers so we can walk up to per-frame calc
            callers=set()
            for cr in ref.getReferencesTo(fn.getEntryPoint()):
                cf=func_at(cr.getFromAddress())
                if cf: callers.add(str(cf.getEntryPoint()))
            w("CALLERS: "+", ".join(sorted(callers)))
            w(decompile(fn))
w("\n===== SEED FUNCTIONS =====")
for s in [0x1eeb0,0x20adc]:
    fn=func_at(a(s))
    if fn:
        w("\n--- seed %s -> %s @ %s ---" % (hex(s), fn.getName(), fn.getEntryPoint()))
        w(decompile(fn))
out.close()
print("DumpEffect: wrote effect_decomp.txt")
