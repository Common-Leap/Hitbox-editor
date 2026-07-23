from unicorn import *
from unicorn.arm64_const import *
import struct
from dump_paths import dump_file
IMG=dump_file('main_reloc.bin').read_bytes()
size=(len(IMG)+0xfff)&~0xfff
STACK=0x7000000000; STACK_SZ=0x200000
RET=0x9000000000
class Emu:
    def __init__(self, trace_writes=False):
        mu=Uc(UC_ARCH_ARM64, UC_MODE_LITTLE_ENDIAN)
        mu.mem_map(0, size); mu.mem_write(0, IMG)
        mu.mem_map(STACK, STACK_SZ)
        self.mu=mu; self.faulted=set(); self.writes=[]; self.trace_writes=trace_writes
        # lazy-map any unmapped access to a fresh zero page so funcs run past bad pointers
        mu.hook_add(UC_HOOK_MEM_READ_UNMAPPED|UC_HOOK_MEM_WRITE_UNMAPPED|UC_HOOK_MEM_FETCH_UNMAPPED, self._fault)
        if trace_writes:
            mu.hook_add(UC_HOOK_MEM_WRITE, self._w)
    def _fault(self, mu, access, addr, size_, val, ud):
        page=addr & ~0xfff
        if page not in self.faulted:
            try: mu.mem_map(page, 0x1000)
            except UcError: pass
            self.faulted.add(page)
        return True  # retry the access
    def _w(self, mu, access, addr, size_, val, ud):
        self.writes.append((mu.reg_read(UC_ARM64_REG_PC), addr, size_, val))
    def call(self, addr, args):
        mu=self.mu
        for i,a in enumerate(args[:8]): mu.reg_write(UC_ARM64_REG_X0+i, a)
        mu.reg_write(UC_ARM64_REG_SP, STACK+STACK_SZ-0x4000)
        mu.reg_write(UC_ARM64_REG_LR, RET)
        try:
            mu.emu_start(addr, RET, count=2000000)
            return ("OK", mu.reg_read(UC_ARM64_REG_X0))
        except UcError as e:
            return ("ERR", str(e), hex(mu.reg_read(UC_ARM64_REG_PC)))
if __name__=="__main__":
    # validate lazy-fault on a real complex function: ParticleSort 0x20ab0
    e=Emu(trace_writes=True)
    r=e.call(0x20ab0, [0,0,0,0,0,0,0])
    print("sort call:", r, "| writes:", len(e.writes), "| pages lazy-mapped:", len(e.faulted))
