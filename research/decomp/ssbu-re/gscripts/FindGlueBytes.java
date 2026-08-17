import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.*;
import ghidra.program.model.mem.*;
import java.io.*;
import java.nio.file.*;

/** Search this program image for the glue byte run captured by DumpGlueBytes. */
public class FindGlueBytes extends GhidraScript {
    public void run() throws Exception {
        String hex = Files.readString(Paths.get("glue_tail_bytes.txt")).trim();
        byte[] pat = new byte[hex.length() / 2];
        for (int i = 0; i < pat.length; i++) {
            pat[i] = (byte) Integer.parseInt(hex.substring(2 * i, 2 * i + 2), 16);
        }
        println("FindGlueBytes: pattern length " + pat.length);
        Memory mem = currentProgram.getMemory();
        Address at = mem.getMinAddress();
        int hits = 0;
        StringBuilder sb = new StringBuilder();
        while (at != null && hits < 20) {
            Address found = mem.findBytes(at, pat, null, true, monitor);
            if (found == null) break;
            hits++;
            sb.append("  hit at ").append(found).append('\n');
            at = found.add(1);
        }
        println("FindGlueBytes: " + hits + " hit(s) in " + currentProgram.getName() + "\n" + sb);
    }
}
