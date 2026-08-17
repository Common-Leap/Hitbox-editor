import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.*;
import ghidra.program.model.mem.*;
import java.io.*;
import java.util.*;

/**
 * Locate ADRP/ADD pairs that materialize a given address, without needing full program analysis.
 *
 * The 13.0.4 image is imported raw, so Ghidra has no references. The Lua binding table is built by
 * code that loads each name string with `adrp`+`add`, so scanning for that pair finds the
 * registration site for a name even in an unanalyzed image.
 */
public class FindStrRefs extends GhidraScript {
    public void run() throws Exception {
        long[] targets = {0x42a2ff9L, 0x4422244L /* set_frame_partial_sync_anim_cmd - 6 */};
        String argTarget = System.getenv("FIND_TARGET");
        if (argTarget != null) targets = new long[] { Long.decode(argTarget) };

        PrintWriter out = new PrintWriter(new FileWriter("strrefs.txt"));
        Memory mem = currentProgram.getMemory();
        MemoryBlock blk = mem.getBlocks()[0];
        long lo = blk.getStart().getOffset();
        long hi = blk.getEnd().getOffset();
        int size = (int) Math.min(hi - lo + 1, Integer.MAX_VALUE - 8);
        byte[] buf = new byte[size];
        mem.getBytes(blk.getStart(), buf);
        println("FindStrRefs: loaded " + size + " bytes");

        for (long target : targets) {
            out.println("\n#### target " + Long.toHexString(target));
            int imm12 = (int) (target & 0xfff);
            long page = target & ~0xfffL;
            // Register-indexed cache of the most recent adrp result, so the add can be matched.
            long[] adrpPage = new long[32];
            long[] adrpAt = new long[32];
            Arrays.fill(adrpPage, -1);
            int found = 0;
            for (int off = 0; off + 4 <= size && found < 40; off += 4) {
                int w = (buf[off] & 0xff) | ((buf[off + 1] & 0xff) << 8)
                        | ((buf[off + 2] & 0xff) << 16) | ((buf[off + 3] & 0xff) << 24);
                if ((w & 0x9f000000) == 0x90000000) { // adrp
                    int rd = w & 0x1f;
                    long immlo = (w >> 29) & 0x3;
                    long immhi = (w >> 5) & 0x7ffff;
                    long imm = (immhi << 2) | immlo;
                    if ((imm & (1L << 20)) != 0) imm |= ~((1L << 21) - 1); // sign extend
                    long pc = lo + off;
                    adrpPage[rd] = (pc & ~0xfffL) + (imm << 12);
                    adrpAt[rd] = pc;
                    continue;
                }
                if ((w & 0xffc00000) == 0x91000000) { // add xd, xn, #imm12
                    int imm = (w >> 10) & 0xfff;
                    int rn = (w >> 5) & 0x1f;
                    int rd = w & 0x1f;
                    if (imm == imm12 && adrpPage[rn] == page) {
                        found++;
                        long pc = lo + off;
                        out.println(String.format("  materialized in x%d at %08x (adrp x%d at %08x)",
                                rd, pc, rn, adrpAt[rn]));
                    }
                    if (rd != rn) adrpPage[rd] = -1;
                }
            }
            out.println("  hits: " + found);
        }
        out.close();
        println("FindStrRefs: done");
    }
}
