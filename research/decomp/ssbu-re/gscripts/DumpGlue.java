import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import java.io.*;

/**
 * Decompile the Lua-facing glue thunks for the partial-frame MotionModule bindings, plus the
 * non-partial `set_frame` glue as a control, so the boolean tail can be read rather than guessed.
 */
public class DumpGlue extends GhidraScript {
    public void run() throws Exception {
        long[] addrs = {
            0x173d400L, // set_frame_partial
            0x173d6b0L, // set_frame_partial_sync_anim_cmd
            0x173d160L, // frame_partial (getter control)
            0x173ccb0L, // end_frame_partial (control)
            0x1738280L, // set_frame (non-partial control)
            0x1738500L, // set_frame_sync_anim_cmd (non-partial control)
        };
        PrintWriter out = new PrintWriter(new FileWriter("glue_decomp.txt"));
        DecompInterface dec = new DecompInterface();
        dec.setOptions(new DecompileOptions());
        dec.openProgram(currentProgram);
        for (long a : addrs) {
            Address addr = toAddr(a);
            Function f = getFunctionAt(addr);
            if (f == null) {
                f = createFunction(addr, null);
            }
            out.println("\n======== " + Long.toHexString(a) + " " + (f == null ? "<no function>" : f.getName()));
            if (f == null) continue;
            DecompileResults res = dec.decompileFunction(f, 180, monitor);
            if (res != null && res.decompileCompleted()) {
                out.println(res.getDecompiledFunction().getC());
            } else {
                out.println("  <decompile failed>");
            }
            out.println("---- raw disassembly ----");
            Listing l = currentProgram.getListing();
            InstructionIterator ii = l.getInstructions(f.getBody(), true);
            while (ii.hasNext()) {
                Instruction in = ii.next();
                out.println("  " + in.getAddress() + "  " + in);
            }
        }
        dec.dispose();
        out.close();
        println("DumpGlue: done");
    }
}
