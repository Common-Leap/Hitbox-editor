import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.symbol.*;
import java.io.*;
import java.util.*;

public class DumpRange extends GhidraScript {
    public void run() throws Exception {
        Listing lst = currentProgram.getListing();
        PrintWriter out = new PrintWriter(new FileWriter("range_out.txt"));
        Scanner sc = new Scanner(new File("targets.txt"));
        while (sc.hasNext()) {
            String tok = sc.next().trim();
            if (tok.isEmpty()) continue;
            long v = Long.decode(tok);
            long start = v - 0x90, end = v + 0x40;
            Address a = toAddr(start);
            out.println("\n===== window around " + Long.toHexString(v) + " =====");
            // ensure disassembled
            try { disassemble(toAddr(start)); } catch (Exception e) {}
            Address cur = toAddr(start);
            while (cur.getOffset() < end) {
                Instruction ins = lst.getInstructionAt(cur);
                if (ins == null) {
                    try { disassemble(cur); } catch (Exception e) {}
                    ins = lst.getInstructionAt(cur);
                }
                if (ins == null) { out.println("  " + cur + ": <no insn>"); cur = cur.add(4); continue; }
                String mark = (cur.getOffset()==v) ? "   <== RET HERE" : "";
                String note = "";
                String m = ins.getMnemonicString();
                if (m.startsWith("bl") || m.equals("b") || m.equals("blr")) {
                    Reference[] refs = ins.getReferencesFrom();
                    for (Reference r : refs) note += " -> " + r.getToAddress();
                }
                out.println("  " + cur + ": " + ins.toString() + note + mark);
                cur = cur.add(ins.getLength());
            }
        }
        out.close(); println("DumpRange done");
    }
}
