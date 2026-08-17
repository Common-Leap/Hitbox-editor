import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.*;
import ghidra.program.model.mem.*;
import java.io.*;

/** Emit the raw bytes of the set_frame_partial Lua glue tail so another program image can be searched for it. */
public class DumpGlueBytes extends GhidraScript {
    public void run() throws Exception {
        // Everything after the one `adrp` in the body, through the `blr`: only intra-function
        // relative branches remain, so these bytes are comparable across binaries.
        long start = 0x173d478L;
        int len = 0x20c;
        Memory mem = currentProgram.getMemory();
        byte[] b = new byte[len];
        mem.getBytes(toAddr(start), b);
        StringBuilder sb = new StringBuilder();
        for (byte x : b) sb.append(String.format("%02x", x));
        PrintWriter out = new PrintWriter(new FileWriter("glue_tail_bytes.txt"));
        out.println(sb);
        out.close();
        println("DumpGlueBytes: " + sb);
    }
}
