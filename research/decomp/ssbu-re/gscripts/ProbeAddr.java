import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.mem.*;
import java.io.*;

public class ProbeAddr extends GhidraScript {
    public void run() throws Exception {
        PrintWriter out = new PrintWriter(new FileWriter("probe_addr.txt"));
        long[] addrs = {0x355f8f0L, 0x3563720L, 0x60bfd8L, 0x35832b6L};
        Memory mem = currentProgram.getMemory();
        out.println("blocks:");
        for (MemoryBlock b : mem.getBlocks()) {
            out.println("  " + b.getName() + " " + b.getStart() + "-" + b.getEnd() + " x=" + b.isExecute());
        }
        for (long a : addrs) {
            Address addr = toAddr(a);
            MemoryBlock b = mem.getBlock(addr);
            out.println("\naddr " + Long.toHexString(a) + " block=" + (b == null ? "NONE" : b.getName()));
            if (b != null) {
                byte[] bytes = new byte[16];
                mem.getBytes(addr, bytes);
                StringBuilder sb = new StringBuilder();
                for (byte x : bytes) sb.append(String.format("%02x ", x));
                out.println("  bytes: " + sb);
                CodeUnit cu = currentProgram.getListing().getCodeUnitAt(addr);
                out.println("  codeunit: " + (cu == null ? "null" : cu.getClass().getSimpleName() + " " + cu));
            }
        }
        out.close();
        println("ProbeAddr: done");
    }
}
