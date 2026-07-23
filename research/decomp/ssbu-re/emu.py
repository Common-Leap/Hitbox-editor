from unicorn import *
from unicorn.arm64_const import *
import struct
from dump_paths import dump_file
IMG=dump_file('main_reloc.bin').read_bytes()
BASE=0x0
# round image size up to page
size=(len(IMG)+0xfff)&~0xfff
STACK=0x7000000000; STACK_SZ=0x100000
HEAP=0x8000000000;  HEAP_SZ=0x100000
RET_MAGIC=0x9000000000
def new_emu():
    mu=Uc(UC_ARCH_ARM64, UC_MODE_LITTLE_ENDIAN)
    mu.mem_map(BASE, size)
    mu.mem_write(BASE, IMG)
    mu.mem_map(STACK, STACK_SZ)
    mu.mem_map(HEAP, HEAP_SZ)
    return mu
def call(mu, addr, args, struct_setup=None):
    for i,a in enumerate(args):
        mu.reg_write(UC_ARM64_REG_X0+i, a)
    mu.reg_write(UC_ARM64_REG_SP, STACK+STACK_SZ-0x1000)
    mu.reg_write(UC_ARM64_REG_LR, RET_MAGIC)
    try:
        mu.emu_start(addr, RET_MAGIC, count=100000)
    except UcError as e:
        return ("ERR", str(e), mu.reg_read(UC_ARM64_REG_PC))
    return ("OK", mu.reg_read(UC_ARM64_REG_X0))
# test 0x6eac0: set [heap+0x18]=7 -> expect 0xc+0xc+8 = 0x20
mu=new_emu()
mu.mem_write(HEAP+0x18, struct.pack('<I',7))
print("flags=7 ->", call(mu,0x6eac0,[HEAP]))
mu=new_emu(); mu.mem_write(HEAP+0x18, struct.pack('<I',1))
print("flags=1 ->", call(mu,0x6eac0,[HEAP]), "(expect 0xc)")
mu=new_emu(); mu.mem_write(HEAP+0x18, struct.pack('<I',0))
print("flags=0 ->", call(mu,0x6eac0,[HEAP]), "(expect 0)")
