import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.*;
import ghidra.program.model.mem.*;
import java.io.*;

/** Print the full NUL-terminated C strings that contain a needle, plus their start addresses. */
public class DumpStrHits extends GhidraScript {
    public void run() throws Exception {
        String needle = "frame_partial";
        String tag = currentProgram.getName().replaceAll("[^A-Za-z0-9_.]", "_");
        PrintWriter out = new PrintWriter(new FileWriter("strhits_" + tag + ".txt"));
        Memory mem = currentProgram.getMemory();
        byte[] pat = needle.getBytes("US-ASCII");
        Address at = mem.getMinAddress();
        int hits = 0;
        while (at != null && hits < 200) {
            Address found = mem.findBytes(at, pat, null, true, monitor);
            if (found == null) break;
            hits++;
            // Walk back to the preceding NUL, then read forward to the terminator.
            long start = found.getOffset();
            while (start > 0) {
                byte b = mem.getByte(toAddr(start - 1));
                if (b == 0 || b < 0x20 || b > 0x7e) break;
                start--;
            }
            StringBuilder sb = new StringBuilder();
            long p = start;
            while (sb.length() < 400) {
                byte b = mem.getByte(toAddr(p));
                if (b == 0 || b < 0x20 || b > 0x7e) break;
                sb.append((char) b);
                p++;
            }
            out.println(String.format("%08x  %s", start, sb));
            at = found.add(1);
        }
        out.println("total: " + hits);
        out.close();
        println("DumpStrHits: " + hits);
    }
}
